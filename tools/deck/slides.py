"""
SANKHYA — to count is to make completely known
Copyright © 2026 Ashutosh Sinha <ajsinha@gmail.com>. All rights reserved.
Proprietary and confidential. See LICENSE and NOTICE at the repository root.

The SANKHYA deck: the name, the foundations, the system, the evidence, and what
an audit of it cost — in five parts.

**Why this is a driver over a directory rather than one file.** A deck of this
length is a source file of several thousand lines, and this repository holds
every source file to fifteen hundred. Splitting by chapter is the same answer
the server's test suite got: cut by subject, because a file cut at the
fifteen-hundredth line is two files nobody can name.

Each chapter is executed into ONE namespace, in order, sharing the helpers and
the running slide counter. A chapter is a script that draws slides, not a module
with an interface, and giving it one would be inventing a contract to satisfy an
import system rather than a reader.
"""
# -*- coding: utf-8 -*-
import os
import pathlib
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
CHAPTERS = os.path.join(HERE, "deck")


def _run(path, namespace):
    with open(path, encoding="utf-8") as handle:
        exec(compile(handle.read(), path, "exec"), namespace)


def build(out_path):
    # `os` and `__file__` are seeded because a chapter resolves asset paths
    # relative to the generator, and an exec'd file gets neither for free.
    namespace = {"__name__": "__deck__", "os": os,
                 "__file__": os.path.join(HERE, "theme.py")}
    _run(os.path.join(HERE, "theme.py"), namespace)
    _run(os.path.join(CHAPTERS, "__preamble__.py"), namespace)
    # Numeric order IS the chapter order, so the file names carry the sequence
    # rather than a list here that can disagree with what is on disk.
    for name in sorted(os.listdir(CHAPTERS)):
        if name.endswith(".py") and not name.startswith("__"):
            _run(os.path.join(CHAPTERS, name), namespace)
    namespace["prs"].save(out_path)
    return len(namespace["prs"].slides._sldIdLst)


# The deck is a document, so it belongs beside the documents. Defaulting to the
# working directory puts a second copy wherever anybody happens to build it,
# and two copies of a long deck differ the moment one of them is rebuilt --- with
# nothing to say which is current.
DEFAULT_OUT = (pathlib.Path(__file__).resolve().parents[2]
               / "docs" / "SANKHYA-Architecture-and-Evidence.pptx")

if __name__ == "__main__":
    target = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_OUT
    target.parent.mkdir(parents=True, exist_ok=True)
    print("slides:", build(str(target)), "->", target)
