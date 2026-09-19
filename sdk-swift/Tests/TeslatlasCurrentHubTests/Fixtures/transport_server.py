import socket
import sys
import time

mode = sys.argv[1]
count = int(sys.argv[2]) if len(sys.argv) > 2 else 1
host = sys.argv[3] if len(sys.argv) > 3 else "127.0.0.1"
family = socket.AF_INET6 if ":" in host else socket.AF_INET

server = socket.socket(family)
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind((host, 0))
server.listen(max(8, count))
print(server.getsockname()[1], flush=True)


def receive_request(connection):
    incoming = b""
    while b"\r\n\r\n" not in incoming and len(incoming) <= 65536:
        part = connection.recv(4096)
        if not part:
            break
        incoming += part


def chunked(connection, status=b"200 OK", chunks=17, delay=0):
    connection.sendall(
        b"HTTP/1.1 " + status + b"\r\n"
        b"Content-Type: application/octet-stream\r\n"
        + (b"WWW-Authenticate: Bearer\r\n" if status.startswith(b"401") else b"")
        + b"Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    )
    chunk = b"x" * 65536
    for _ in range(chunks):
        connection.sendall(b"10000\r\n" + chunk + b"\r\n")
        if delay:
            time.sleep(delay)
    connection.sendall(b"0\r\n\r\n")


try:
    for _ in range(count):
        connection, _ = server.accept()
        try:
            receive_request(connection)
            if mode == "exact":
                connection.sendall(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n"
                    b"X-Request-ID: linux-exact\r\nConnection: close\r\n\r\n1234"
                )
            elif mode == "redirect":
                connection.sendall(
                    b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/escaped\r\n"
                    b"Content-Length: 0\r\nConnection: close\r\n\r\n"
                )
            elif mode == "large-header":
                connection.sendall(
                    b"HTTP/1.1 200 OK\r\nX-Large: " + b"x" * 70000
                    + b"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
            elif mode == "error-oversize":
                chunked(connection, status=b"401 Unauthorized")
            elif mode == "cancel-during":
                chunked(connection, chunks=100, delay=0.02)
            elif mode == "timeout":
                time.sleep(2)
                connection.sendall(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n1234"
                )
            elif mode == "informational":
                connection.sendall(
                    b"HTTP/1.1 103 Early Hints\r\n"
                    b"content-type: text/plain\r\ncache-control: public\r\n"
                    b"etag: \"stale\"\r\nLink: </early>; rel=preload\r\n\r\n"
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"
                    b"CACHE-CONTROL: no-store\r\n"
                    b"eTaG: \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"\r\n"
                    b"Content-Length: 2\r\nConnection: close\r\n\r\n{}"
                )
            elif mode == "conflicting-singleton":
                connection.sendall(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"
                    b"content-type: text/plain\r\nContent-Length: 2\r\n"
                    b"Connection: close\r\n\r\n{}"
                )
            elif mode == "repeated-list-fields":
                connection.sendall(
                    b"HTTP/1.1 401 Unauthorized\r\n"
                    b"WWW-Authenticate: Bearer realm=\"hub\"\r\n"
                    b"www-authenticate: Basic realm=\"fallback\"\r\n"
                    b"Cache-Control: private\r\ncache-control: no-store\r\n"
                    b"X-Request-ID: repeated-auth\r\n"
                    b"Content-Length: 0\r\nConnection: close\r\n\r\n"
                )
            elif mode == "auth-401":
                connection.sendall(
                    b"HTTP/1.1 401 Unauthorized\r\n"
                    b"WWW-Authenticate: Bearer realm=\"hub\"\r\n"
                    b"Cache-Control: private, no-store\r\n"
                    b"X-Request-ID: loopback-auth\r\n"
                    b"Content-Length: 0\r\nConnection: close\r\n\r\n"
                )
            elif mode == "trailers":
                connection.sendall(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"
                    b"ETag: \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"\r\n"
                    b"Transfer-Encoding: chunked\r\nTrailer: ETag, X-Trailer-Only\r\n"
                    b"Connection: close\r\n\r\n2\r\n{}\r\n0\r\n"
                    b"ETag: \"trailer-must-not-win\"\r\nX-Trailer-Only: ignored\r\n\r\n"
                )
            else:
                raise RuntimeError("unknown mode")
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            connection.close()
finally:
    server.close()
