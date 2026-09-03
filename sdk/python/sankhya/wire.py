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

Transport security
------------------
Built 2026-09-03. The server has offered TLS since 2026-08-31 and this binding did not ask for
it, so a connection from anywhere but a loopback sent its password as typed.

``sslmode`` takes PostgreSQL's own vocabulary, because a person configuring a client already
knows it and a second vocabulary for one idea is one somebody gets wrong:

============== ===============================================================================
``disable``    Never ask. The connection is in the clear and says so.
``prefer``     Ask; encrypt if the server offers it, continue if it does not.
``require``    Ask, and **refuse** if the server declines. No verification of who answered.
``verify-ca``  ``require``, and the server's certificate must be signed by ``sslrootcert``.
``verify-full`` ``verify-ca``, and the certificate must be *for the host that was asked for*.
============== ===============================================================================

The default is ``prefer``, which is libpq's, and the honesty is elsewhere:
:attr:`Connection.encrypted` says what actually happened. A binding that *silently* downgrades
is the thing this repository refuses; one that downgrades and reports it is a client whose
posture a caller can assert on --- and the quickstart does.

What it does not do
-------------------
The extended query protocol, binary result formats, and COPY. Each is a real gap and each is
named rather than half-implemented.
"""

from __future__ import annotations

import socket
import ssl
import struct
from dataclasses import dataclass, field
from typing import Iterator

#: The protocol version this speaks: 3.0, as every PostgreSQL since 7.4.
PROTOCOL_VERSION = 196608

#: The message that asks a server whether it speaks TLS.
#:
#: Sent before the startup message and outside the ordinary framing: eight bytes, a length and
#: a magic number, answered with a single ``S`` or ``N``. It is the same handshake every
#: PostgreSQL client performs, which is why a server that declines can still be talked to.
SSL_REQUEST_CODE = 80877103

#: What ``sslmode`` may be, weakest first. Ordered, because `require` is `prefer` plus a refusal
#: and `verify-full` is `verify-ca` plus a name check, and writing that as an order keeps the
#: comparisons below from becoming a table of special cases.
SSL_MODES = ("disable", "prefer", "require", "verify-ca", "verify-full")

#: The client contract this binding speaks.
#:
#: Declared at connection, so a server speaking a different one refuses **there**, naming both
#: versions, rather than eleven calls later when a field turns out to be missing (``ADR-0017``
#: Decision 5). A binding is installed independently of the server --- a package index, a
#: container image and a deployment all move at their own pace --- so the two will disagree,
#: and the only question is where.
CONTRACT_VERSION = 1

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
                 timeout: float = DEFAULT_TIMEOUT, sslmode: str = "prefer",
                 sslrootcert: str | None = None) -> None:
        if sslmode not in SSL_MODES:
            raise ValueError(
                f"sslmode must be one of {', '.join(SSL_MODES)}, and {sslmode!r} is not. "
                f"Refused rather than treated as the default: a mode nobody recognises is "
                f"most likely a stronger one than this binding would have chosen"
            )
        self._socket = socket.create_connection((host, port), timeout=timeout)
        self._socket.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        #: Whether this connection is encrypted. Read it rather than assuming.
        self.encrypted = False
        self._buffer = b""
        self._parameters: dict = {}
        # Bytes over the socket, counted rather than estimated.
        #
        # Not instrumentation for its own sake: the reason to compute in the warehouse is that
        # only the *answers* cross the wire, and that is a claim until somebody measures it. A
        # counter here lets an example show the difference instead of asserting it.
        self._sent = 0
        self._received = 0
        self._negotiate(host, sslmode, sslrootcert)
        self._startup(user, database, password)

    def _negotiate(self, host: str, sslmode: str, sslrootcert: str | None) -> None:
        """Ask the server for TLS, and act on what it says.

        The exchange is deliberately outside the ordinary framing --- it happens before either
        side knows the other speaks this protocol --- so it is eight bytes out and one byte
        back, and the byte is read directly rather than through the frame reader.
        """
        if sslmode == "disable":
            return

        self._socket.sendall(struct.pack("!ii", 8, SSL_REQUEST_CODE))
        self._sent += 8
        answer = self._socket.recv(1)
        self._received += len(answer)

        if answer != b"S":
            if sslmode == "prefer":
                # Declined, and the caller said they would take either. `encrypted` stays
                # False, which is the whole of the honesty: nothing here pretends.
                return
            self._socket.close()
            raise ConnectionError(
                f"sslmode={sslmode} and this server declined TLS. Refused rather than "
                f"continued in the clear: a password sent as typed cannot be un-sent, and a "
                f"client that downgrades silently is how that happens. Configure "
                f"`server.tls.certificate` and `server.tls.private_key`, or connect with "
                f"sslmode=prefer if this connection is a loopback and you mean it"
            )

        # `require` encrypts and does not verify, which is libpq's meaning and is worth being
        # explicit about: it stops a passive listener and not an active one. Verification needs
        # something to verify *against*, and that is `sslrootcert`.
        verifying = sslmode in ("verify-ca", "verify-full")
        context = ssl.create_default_context(cafile=sslrootcert) if verifying \
            else ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        if verifying:
            if sslrootcert is None:
                self._socket.close()
                raise ValueError(
                    f"sslmode={sslmode} verifies the server's certificate and there is nothing "
                    f"to verify it against. Pass `sslrootcert=` the authority that signed it"
                )
            context.check_hostname = sslmode == "verify-full"
            context.verify_mode = ssl.CERT_REQUIRED
        else:
            context.check_hostname = False
            context.verify_mode = ssl.CERT_NONE

        self._socket = context.wrap_socket(
            self._socket, server_hostname=host if sslmode == "verify-full" else None
        )
        self.encrypted = True

    # -- connecting ---------------------------------------------------------

    def _startup(self, user: str, database: str, password: str | None) -> None:
        body = struct.pack("!i", PROTOCOL_VERSION)
        for key, value in (
            ("user", user),
            ("database", database),
            # Declared, so a server speaking another contract refuses at connection rather
            # than serving a client that will misread its answers later.
            ("sankhya_contract", str(CONTRACT_VERSION)),
        ):
            body += key.encode() + b"\0" + value.encode() + b"\0"
        body += b"\0"
        startup = struct.pack("!i", len(body) + 4) + body
        self._sent += len(startup)
        self._socket.sendall(startup)

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
    def bytes_sent(self) -> int:
        """Bytes this connection has put on the wire."""
        return self._sent

    @property
    def bytes_received(self) -> int:
        """Bytes this connection has taken off the wire.

        The number that makes the case for computing in the warehouse: a statement that takes a
        quantile of a stored vector receives one number per row, and the same answer reached by
        fetching the vectors receives every outcome. The difference is not an argument --- it is
        a measurement, and this is what measures it.
        """
        return self._received

    @property
    def parameters(self) -> dict:
        """What the server announced about itself at startup."""
        return dict(self._parameters)

    @property
    def contract(self) -> int | None:
        """The client contract the server speaks, or ``None`` if it announced none.

        ``None`` means this is not a SANKHYA --- something else speaking the PostgreSQL wire
        protocol, which is a thing this binding can talk to and should not pretend otherwise.
        """
        announced = self._parameters.get("sankhya_contract")
        if announced is None:
            return None
        try:
            return int(announced)
        except ValueError:
            return None

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
        message = tag + struct.pack("!i", len(body) + 4) + body
        self._sent += len(message)
        self._socket.sendall(message)

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
            self._received += len(chunk)
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
