import json
import os
import socket
import struct
import subprocess


HEADER_LIMIT = 16 * 1024 * 1024
PAYLOAD_LIMIT = 16 * 1024 * 1024 * 1024
DEFAULT_TIMEOUT_SECONDS = 0.8


class OxidexError(Exception):
    pass


def default_daemon_socket_path():
    runtime_dir = os.environ.get("XDG_RUNTIME_DIR")
    if runtime_dir:
        return os.path.join(runtime_dir, "oxidex", "oxidexd.sock")
    return os.path.join("/run", "user", str(os.getuid()), "oxidex", "oxidexd.sock")


def start_daemon_once():
    return subprocess.Popen(
        ["oxidexd", "--foreground"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        close_fds=True,
    )


class OxidexClient(object):
    def __init__(self, stream):
        self._stream = stream
        self._next_id = 1

    @classmethod
    def connect(cls, socket_path=None, timeout=DEFAULT_TIMEOUT_SECONDS):
        path = socket_path or default_daemon_socket_path()
        stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        stream.settimeout(timeout)
        try:
            stream.connect(path)
        except Exception:
            stream.close()
            raise
        return cls(stream)

    def close(self):
        try:
            self._stream.close()
        except Exception:
            pass

    def request(self, method, params=None):
        request_id = self._next_id
        self._next_id += 1
        header = {
            "id": request_id,
            "method": method,
            "params": params or {},
        }
        self._write_frame(header, b"")

        while True:
            response, _payload = self._read_frame()
            if response.get("id") != request_id:
                continue
            if response.get("ok") is not True:
                error = response.get("error") or {}
                message = error.get("message") or "daemon request failed"
                code = error.get("code")
                if code:
                    raise OxidexError("%s: %s" % (code, message))
                raise OxidexError(message)
            return response.get("result")

    def status(self):
        return self.request("daemon.status")

    def devices(self):
        return self.request("device.list") or []

    def indexes(self):
        return self.request("index.list") or []

    def jobs(self):
        return self.request("index.job_list") or []

    def search(self, query, limit):
        return self.request(
            "search.query",
            {
                "query": query,
                "request": None,
                "device_filter": None,
                "sort_key": "relevance",
                "sort_direction": "asc",
                "max_results": limit,
            },
        ) or {"rows": [], "truncated": False}

    def resolve_path(self, device_id, record_idx):
        return self.request(
            "open.resolve_path",
            {
                "device_id": device_id,
                "record_idx": record_idx,
            },
        )

    def start_scan(self, device_id):
        return self.request("index.start_scan", {"device_id": device_id})

    def _write_frame(self, header, payload):
        header_bytes = json.dumps(header, separators=(",", ":")).encode("utf-8")
        if len(header_bytes) > HEADER_LIMIT:
            raise OxidexError("IPC header is too large")
        if len(payload) > PAYLOAD_LIMIT:
            raise OxidexError("IPC payload is too large")
        frame_prefix = struct.pack("<IQ", len(header_bytes), len(payload))
        self._stream.sendall(frame_prefix + header_bytes + payload)

    def _read_frame(self):
        prefix = self._read_exact(12)
        header_len, payload_len = struct.unpack("<IQ", prefix)
        if header_len > HEADER_LIMIT:
            raise OxidexError("IPC header length is too large")
        if payload_len > PAYLOAD_LIMIT:
            raise OxidexError("IPC payload is too large")
        header = json.loads(self._read_exact(header_len).decode("utf-8"))
        payload = self._read_exact(payload_len)
        return header, payload

    def _read_exact(self, length):
        chunks = []
        remaining = length
        while remaining:
            chunk = self._stream.recv(remaining)
            if not chunk:
                raise OxidexError("daemon closed the connection")
            chunks.append(chunk)
            remaining -= len(chunk)
        return b"".join(chunks)
