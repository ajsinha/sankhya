"""The PostgreSQL wire protocol, in pure Python.

Why this exists
---------------
``ADR-0017`` Decision 7 makes the Python binding **pure Python**: no compiled extension, no
per-platform wheel matrix, no ABI to keep in step with three interpreter versions. The columnar
door speaks Arrow Flight and needs ``pyarrow``; this module speaks the wire-protocol door and
needs nothing at all, which makes it the floor a client can always stand on --- a laptop with a
stock interpreter and no build toolchain can still connect.

It is deliberately small. ``ADR-0017`` Decision 1 says a binding may contain no logic the server
does not enforce, so this parses frames and does not interpret them: a refusal arrives as its
fields rather than as an exception this module invented.

What it does not do
-------------------
The extended query protocol, binary result formats, COPY, and TLS. Each is a real gap and each
is named rather than half-implemented: a client that silently downgrades is worse than one that
says it cannot.
"""

from __future__ import annotations

import socket
import struct
from dataclasses import dataclass, field
from typing import Iterator

#: The protocol version this speaks: 3.0, as every PostgreSQL since 7.4.
PROTOCOL_VERSION = 196608

#: How long to wait for the server on any single read, in seconds.
#:
#: Bounded, and the bound is part of the contract rather than a precaution. An unbounded read
#: against a server that has stopped answering hangs the caller with no message, which is
#: strictly worse than an error naming the wait.
DEFAULT_TIMEOUT = 30.0


class WireError(Exception):
    """The connection could not be used as a connection."""


@dataclass
class Refusal(Exception):
    """A refusal, as its fields rather than as a sentence.

    ``ADR-0017`` Decision 2: a refusal carries a code a client dispatches on, a SQLSTATE a
    generic driver understands, the remediation that says what to do, and the names it cites.
    A client that has to parse names out of prose turns the message into an API nobody may
    reword.
    """

    sqlstate: str = ""
    message: str = ""
    #: What to do about it.
    detail: str = ""
    #: The **names** this refusal cites: the clones that would break, the two tables a name
    #: could mean, the feed that is not declared.
    #:
    #: The field ``ADR-0017`` Decision 2 calls cheap now and expensive later. Without it a
    #: client showing *"three clones read this table"* has to parse the message --- and the
    #: message then becomes an API nobody meant to publish and nobody may reword.
    subjects: list = field(default_factory=list)
    #: Every field the server sent, by its protocol tag, for anything not named above.
    fields: dict = field(default_factory=dict)

    @property
    def hint(self) -> str:
        """The raw ``H`` field, which is how ``subjects`` crosses the wire."""
        return self.fields.get("H", "")

    def __str__(self) -> str:
        parts = [f"[{self.sqlstate}] {self.message}"]
        if self.detail:
            parts.append(self.detail)
        if self.hint:
            parts.append(self.hint)
        return " -- ".join(parts)


@dataclass
class Result:
    """One statement's answer."""

    #: Column names, in order.
    columns: list
    #: Rows, each a list of ``str`` or ``None``. ``None`` is SQL NULL; ``""`` is the empty
    #: string, and conflating the two is a wrong answer rather than a formatting slip.
    rows: list
    #: The command tag the server closed with, e.g. ``SELECT 3``.
    tag: str = ""


class Connection:
    """One connection to one server."""

    def __init__(self, host: str = "127.0.0.1", port: int = 5432, user: str = "sankhya",
                 database: str = "sankhya", password: str | None = None,
                 timeout: float = DEFAULT_TIMEOUT) -> None:
        self._socket = socket.create_connection((host, port), timeout=timeout)
        self._socket.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        self._buffer = b""
        self._parameters: dict = {}
        self._startup(user, database, password)

    # -- connecting ---------------------------------------------------------

    def _startup(self, user: str, database: str, password: str | None) -> None:
        body = struct.pack("!i", PROTOCOL_VERSION)
        for key, value in (("user", user), ("database", database)):
            body += key.encode() + b"\0" + value.encode() + b"\0"
        body += b"\0"
        self._socket.sendall(struct.pack("!i", len(body) + 4) + body)

        while True:
            tag, payload = self._read_message()
            if tag == b"R":
                kind = struct.unpack("!i", payload[:4])[0]
                if kind == 0:
                    continue
                if kind == 3:
                    if password is None:
                        raise WireError("the server asked for a password and none was given")
                    self._send(b"p", password.encode() + b"\0")
                    continue
                raise WireError(f"unsupported authentication request: {kind}")
            if tag == b"S":
                name, value = payload.split(b"\0")[:2]
                self._parameters[name.decode()] = value.decode()
            elif tag == b"E":
                raise self._refusal(payload)
            elif tag == b"Z":
                return

    @property
    def parameters(self) -> dict:
        """What the server announced about itself at startup."""
        return dict(self._parameters)

    # -- running statements -------------------------------------------------

    def execute(self, sql: str) -> Result:
        """Run one statement and return its rows.

        Raises :class:`Refusal` if the server refused it, carrying the fields rather than a
        rendered string --- which is what lets a caller branch on the SQLSTATE instead of
        matching on prose.
        """
        results = list(self.stream(sql))
        if not results:
            return Result(columns=[], rows=[], tag="")
        return results[-1]

    def stream(self, sql: str) -> Iterator[Result]:
        """Run one statement and yield a result per answer it produces.

        The simple-query protocol allows several, and a client that returned only the first
        would silently drop the rest.
        """
        self._send(b"Q", sql.encode() + b"\0")

        columns: list = []
        rows: list = []
        refusal: Refusal | None = None
        while True:
            tag, payload = self._read_message()
            if tag == b"T":
                columns = self._row_description(payload)
                rows = []
            elif tag == b"D":
                rows.append(self._data_row(payload))
            elif tag == b"C":
                yield Result(columns=columns, rows=rows, tag=payload.split(b"\0")[0].decode())
                columns, rows = [], []
            elif tag == b"I":
                yield Result(columns=[], rows=[], tag="")
            elif tag == b"E":
                refusal = self._refusal(payload)
            elif tag == b"Z":
                if refusal is not None:
                    raise refusal
                return

    def close(self) -> None:
        """Say goodbye, then hang up."""
        try:
            self._send(b"X", b"")
        except OSError:
            pass
        self._socket.close()

    def __enter__(self) -> "Connection":
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    # -- framing ------------------------------------------------------------

    def _send(self, tag: bytes, body: bytes) -> None:
        self._socket.sendall(tag + struct.pack("!i", len(body) + 4) + body)

    def _read_message(self) -> tuple:
        header = self._read_exactly(5)
        tag = header[:1]
        length = struct.unpack("!i", header[1:])[0]
        if length < 4:
            raise WireError(f"a message of {length} bytes cannot contain its own length")
        return tag, self._read_exactly(length - 4)

    def _read_exactly(self, count: int) -> bytes:
        while len(self._buffer) < count:
            chunk = self._socket.recv(65536)
            if not chunk:
                raise WireError("the server closed the connection")
            self._buffer += chunk
        out, self._buffer = self._buffer[:count], self._buffer[count:]
        return out

    @staticmethod
    def _row_description(payload: bytes) -> list:
        count = struct.unpack("!H", payload[:2])[0]
        names, at = [], 2
        for _ in range(count):
            end = payload.index(b"\0", at)
            names.append(payload[at:end].decode())
            # name, then table oid, column, type oid, size, modifier, format: 18 bytes.
            at = end + 1 + 18
        return names

    @staticmethod
    def _data_row(payload: bytes) -> list:
        count = struct.unpack("!H", payload[:2])[0]
        values, at = [], 2
        for _ in range(count):
            length = struct.unpack("!i", payload[at:at + 4])[0]
            at += 4
            if length < 0:
                values.append(None)
                continue
            values.append(payload[at:at + length].decode("utf-8", "replace"))
            at += length
        return values

    @staticmethod
    def _refusal(payload: bytes) -> Refusal:
        fields = {}
        for part in payload.split(b"\0"):
            if len(part) > 1:
                fields[part[:1].decode()] = part[1:].decode("utf-8", "replace")
        return Refusal(
            sqlstate=fields.get("C", ""),
            message=fields.get("M", ""),
            detail=fields.get("D", ""),
            # PostgreSQL has no field for a list, so the names arrive space-separated in the
            # hint field. Split here rather than in every caller: a client branching on which
            # clones would break should not also be parsing a protocol field.
            subjects=fields.get("H", "").split(),
            fields=fields,
        )


def connect(**kwargs) -> Connection:
    """Open a connection. See :class:`Connection` for the arguments."""
    return Connection(**kwargs)
