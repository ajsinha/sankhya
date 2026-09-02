"""The SANKHYA Python binding.

``ADR-0017`` governs what may be in here: **no logic the server does not enforce**. The test is
that deleting this package changes nothing about what the system permits, refuses or audits.
Anything that fails that test is server work wearing a client's clothes.
"""

from .wire import Connection, Refusal, Result, WireError, connect

__all__ = ["Connection", "Refusal", "Result", "WireError", "connect"]
__version__ = "0.1.0"
