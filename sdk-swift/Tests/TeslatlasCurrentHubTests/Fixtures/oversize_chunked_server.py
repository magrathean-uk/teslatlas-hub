import socket
import sys

server = socket.socket()
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", 0))
server.listen(1)
print(server.getsockname()[1], flush=True)
connection, _ = server.accept()
try:
    incoming = b""
    while b"\r\n\r\n" not in incoming:
        part = connection.recv(4096)
        if not part:
            break
        incoming += part
    connection.sendall(
        b"HTTP/1.1 200 OK\r\n"
        b"Content-Type: application/octet-stream\r\n"
        b"Transfer-Encoding: chunked\r\n"
        b"Connection: close\r\n\r\n"
    )
    chunk = b"x" * 65536
    for _ in range(17):
        connection.sendall(b"10000\r\n" + chunk + b"\r\n")
    connection.sendall(b"0\r\n\r\n")
except (BrokenPipeError, ConnectionResetError):
    pass
finally:
    connection.close()
    server.close()
    sys.exit(0)
