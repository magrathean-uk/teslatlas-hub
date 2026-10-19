"""Strict JSON, private-file and deadline-bounded HTTPS helpers.

Shared by `conformance.hub_http_12` and `tools/check-current-hub`. The checks of
the current Hub profile itself live in `hub_http_12`.
"""
from decimal import Decimal, DecimalException
import http.client
import io
import json
import math
import os
import select
import socket
import ssl
import stat
import time
import urllib.parse

MAX_BYTES=1048576


class AcceptanceError(Exception):
    """Messages are fixed labels; never interpolate server data or credentials."""

class _RoundedFraction(float):
    """Finite float whose raw mathematical value was not an integer.

    JSON Schema's integer checker calls is_integer on floats. Retain this
    distinction while preserving ordinary finite float use and serialization.
    """
    def is_integer(self):
        return False

def strict_json(raw):
    def unique(pairs):
        result={}
        for key,value in pairs:
            if key in result:raise ValueError('duplicate member')
            result[key]=value
        return result
    def reject(_):raise ValueError('non-finite number')
    def finite(value):
        # JSON Schema integer includes integral decimal/exponent spellings.
        # Preserve their mathematical value before binary64 normalization.
        try:
            exact=Decimal(value)
        except DecimalException:
            raise ValueError('unsupported numeric exponent') from None
        parsed=float(value)
        if not math.isfinite(parsed):raise ValueError('overflowing number')
        # Finite binary64 conversion bounds allocation for integral exponents.
        # Field schemas enforce signed64 only where the field is an integer.
        if exact == exact.to_integral_value():return int(exact)
        if parsed.is_integer():return _RoundedFraction(parsed)
        return parsed
    return json.loads(raw,object_pairs_hook=unique,parse_constant=reject,parse_float=finite)

def read_private(path):
    fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
    with os.fdopen(fd,'rb') as stream:
        meta=os.fstat(stream.fileno())
        if not stat.S_ISREG(meta.st_mode) or meta.st_uid!=os.getuid() or meta.st_mode & 0o077:
            raise AcceptanceError('private file permissions invalid')
        raw=stream.read(65537)
    if len(raw)>65536:raise AcceptanceError('private file exceeds limit')
    return strict_json(raw)

def canonical_origin(endpoint):
    if not isinstance(endpoint,str):raise AcceptanceError('owned loopback HTTPS endpoint required')
    url=urllib.parse.urlsplit(endpoint)
    if url.scheme!='https' or url.hostname not in ('127.0.0.1','localhost','::1') or url.username or url.password or url.query or url.fragment or url.path not in ('','/'):
        raise AcceptanceError('owned loopback HTTPS endpoint required')
    return endpoint.rstrip('/')

def _remaining(deadline, clock=time.monotonic):
    value = deadline - clock()
    if value <= 0:
        raise TimeoutError("total exchange deadline expired")
    return value


class _DeadlineSocketReader(io.RawIOBase):
    def __init__(self, connection, deadline, clock):
        super().__init__()
        self.connection = connection
        self.deadline = deadline
        self.clock = clock
        self._lease = connection.makefile("rb", buffering=0)

    def close(self):
        try:
            self._lease.close()
        finally:
            super().close()

    def readable(self):
        return True

    def readinto(self, buffer):
        self.connection.settimeout(_remaining(self.deadline, self.clock))
        try:
            return self._lease.readinto(buffer)
        except socket.timeout:
            raise TimeoutError("total exchange deadline expired") from None


class _DeadlineResponseSocket:
    def __init__(self, connection, deadline, clock):
        self.connection = connection
        self.deadline = deadline
        self.clock = clock

    def makefile(self, mode):
        if mode != "rb":
            raise ValueError("HTTP response stream mode is invalid")
        return io.BufferedReader(_DeadlineSocketReader(self.connection, self.deadline, self.clock))


class _DeadlineHTTPSConnection(http.client.HTTPSConnection):
    """Loopback HTTPS connection whose blocking boundaries share one deadline."""

    def __init__(self, host, port, *, context, deadline, clock):
        self.deadline = deadline
        self.clock = clock
        super().__init__(
            host,
            port,
            timeout=_remaining(deadline, clock),
            context=context,
        )

    def _addresses(self):
        if self.host == "127.0.0.1":
            return ((socket.AF_INET, ("127.0.0.1", self.port)),)
        if self.host == "::1":
            return ((socket.AF_INET6, ("::1", self.port, 0, 0)),)
        return (
            (socket.AF_INET6, ("::1", self.port, 0, 0)),
            (socket.AF_INET, ("127.0.0.1", self.port)),
        )

    def _connect_tcp(self):
        last_error = None
        for family, address in self._addresses():
            connection = socket.socket(family, socket.SOCK_STREAM)
            try:
                if self.source_address:
                    connection.bind(self.source_address)
                connection.settimeout(_remaining(self.deadline, self.clock))
                connection.connect(address)
                _remaining(self.deadline, self.clock)
                try:
                    connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
                except OSError:
                    pass
                return connection
            except (OSError, TimeoutError) as error:
                last_error = error
                connection.close()
        if last_error is not None:
            raise last_error
        raise OSError("loopback endpoint has no address")

    def _wrap_tls(self, connection):
        connection.settimeout(_remaining(self.deadline, self.clock))
        wrapped = self._context.wrap_socket(
            connection,
            server_hostname=self.host,
            do_handshake_on_connect=False,
        )
        try:
            wrapped.setblocking(False)
            while True:
                try:
                    wrapped.do_handshake()
                    break
                except ssl.SSLWantReadError:
                    if not select.select([wrapped], [], [], _remaining(self.deadline, self.clock))[0]:
                        raise TimeoutError("total exchange deadline expired")
                except ssl.SSLWantWriteError:
                    if not select.select([], [wrapped], [], _remaining(self.deadline, self.clock))[1]:
                        raise TimeoutError("total exchange deadline expired")
            wrapped.settimeout(_remaining(self.deadline, self.clock))
            return wrapped
        except BaseException:
            wrapped.close()
            raise

    def connect(self):
        connection = self._connect_tcp()
        try:
            _remaining(self.deadline, self.clock)
            self.sock = self._wrap_tls(connection)
        except BaseException:
            connection.close()
            raise

    def send(self, data):
        if self.sock is None:
            if self.auto_open:
                self.connect()
            else:
                raise http.client.NotConnected()
        if not data:
            return
        remaining = memoryview(data)
        self.sock.setblocking(False)
        try:
            while remaining:
                try:
                    sent = self.sock.send(remaining)
                    if sent == 0:
                        raise OSError("TLS socket closed during request send")
                    remaining = remaining[sent:]
                except (BlockingIOError, ssl.SSLWantWriteError):
                    if not select.select([], [self.sock], [], _remaining(self.deadline, self.clock))[1]:
                        raise TimeoutError("total exchange deadline expired")
                except ssl.SSLWantReadError:
                    if not select.select([self.sock], [], [], _remaining(self.deadline, self.clock))[0]:
                        raise TimeoutError("total exchange deadline expired")
            self.sock.settimeout(_remaining(self.deadline, self.clock))
        except BaseException:
            remaining.release()
            raise
        remaining.release()
