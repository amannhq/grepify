"""
Grepify is a lightweight code index for any harness.
"""

from ._version import __version__ as __version__


# Re-export APIs from internal modules

from . import _internal
from ._internal.api import *  # noqa: F403

__all__ = _internal.api.__all__
