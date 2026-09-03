"""The worker: SANKHYA's half of the contract, inside the sandbox.

This file is shipped by the server and is the only Python it runs that it wrote itself. The
author's code is *data* to it --- it arrives in the request, is executed in a namespace of this
module's making, and never sees this module's own globals.

# Why a frame rather than a line-based protocol

Because the values are a buffer. `ADR-0010` requires that a batch cross the boundary rather than
a row --- an interpreter boundary crossed per row is crossed a hundred million times in a scan
--- and a batch of doubles is not text. So the request is length-prefixed binary and the values
arrive as a `memoryview` of doubles, which NumPy reads with `frombuffer` at no copy and which the
standard library iterates without materialising a list.

# What the author writes

    def initial():                    # optional; None if absent
    def accumulate(state, values):    # required; `values` is a memoryview of doubles
    def merge(a, b):                  # optional --- its presence IS the composability claim
    def finish(state):                # required; returns a float

The state is JSON. `ADR-0010` says a measure whose state is not serialisable is usable and not
materialisable, and says so *at declaration*; JSON is what makes that checkable in one line
rather than a property discovered when a cuboid fails to write.
"""

import json
import struct
import sys

MAGIC = 0x474B_534B
VERSION = 1

ACCUMULATE, MERGE, FINISH, DESCRIBE = 1, 2, 3, 4
IS_STATE, IS_NUMBER, IS_REFUSAL = 1, 2, 3


def read_request(stream):
    """The whole request, as its four parts."""
    header = stream.read(12)
    if len(header) != 12:
        raise ValueError("the request ended before its header")
    magic, version, operation, parts = struct.unpack("<IHHI", header)
    if magic != MAGIC:
        raise ValueError("this is not a request from SANKHYA")
    if version != VERSION:
        raise ValueError(f"this worker speaks version {VERSION} and was sent {version}")
    lengths = struct.unpack(f"<{parts}Q", stream.read(8 * parts))
    return operation, [stream.read(length) for length in lengths]


def write_response(stream, kind, payload):
    stream.write(struct.pack("<IHH", MAGIC, VERSION, kind))
    stream.write(struct.pack("<Q", len(payload)))
    stream.write(payload)
    stream.flush()


def author_module(source):
    """The author's code, executed in a namespace of our making.

    Not a security boundary and not pretending to be one: `ADR-0023` Decision 1 puts the
    boundary in the kernel, outside this process entirely. This namespace exists so that the
    author's names do not collide with this file's.
    """
    namespace = {"__name__": "sankhya_user_function", "__builtins__": __builtins__}
    exec(compile(source, "<the function as declared>", "exec"), namespace)  # noqa: S102
    return namespace


def as_state(value):
    """A state, as bytes. Refuses what cannot be stored rather than storing what cannot be read."""
    try:
        return json.dumps(value).encode()
    except (TypeError, ValueError) as why:
        raise ValueError(
            f"this aggregation's state is not JSON, so it cannot be materialised: {why}"
        ) from why


def main():
    stream_in = sys.stdin.buffer
    stream_out = sys.stdout.buffer
    try:
        operation, parts = read_request(stream_in)
        source = parts[0].decode()
        namespace = author_module(source)

        if operation == DESCRIBE:
            write_response(
                stream_out,
                IS_STATE,
                json.dumps(
                    {name: callable(namespace.get(name))
                     for name in ("initial", "accumulate", "merge", "finish")}
                ).encode(),
            )
            return 0

        if operation == ACCUMULATE:
            accumulate = namespace.get("accumulate")
            if not callable(accumulate):
                raise ValueError("this aggregation declares no `accumulate`")
            if parts[1]:
                state = json.loads(parts[1].decode())
            else:
                initial = namespace.get("initial")
                state = initial() if callable(initial) else None
            values = memoryview(parts[2]).cast("d")
            write_response(stream_out, IS_STATE, as_state(accumulate(state, values)))
            return 0

        if operation == MERGE:
            merge = namespace.get("merge")
            if not callable(merge):
                raise ValueError(
                    "this aggregation declares no `merge`, so its partial results cannot be "
                    "combined and it does not compose"
                )
            left = json.loads(parts[1].decode())
            right = json.loads(parts[2].decode())
            write_response(stream_out, IS_STATE, as_state(merge(left, right)))
            return 0

        if operation == FINISH:
            finish = namespace.get("finish")
            if not callable(finish):
                raise ValueError("this aggregation declares no `finish`")
            answer = float(finish(json.loads(parts[1].decode())))
            write_response(stream_out, IS_NUMBER, struct.pack("<d", answer))
            return 0

        raise ValueError(f"unknown operation {operation}")
    except Exception as why:  # noqa: BLE001 -- every failure is reported, never raised at a pipe
        write_response(stream_out, IS_REFUSAL, f"{type(why).__name__}: {why}".encode())
        return 0


if __name__ == "__main__":
    sys.exit(main())
