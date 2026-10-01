"""`python3 bench/cb ...` (the directory) or `python3 -m cb` with bench/ on
sys.path."""

import os
import sys

if __package__ in (None, ""):
    # Run as a directory, sys.path[0] is bench/cb itself; replace it with
    # bench/ so the modules import as the cb package.
    sys.path[0] = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    from cb.cli import main
else:
    from .cli import main

sys.exit(main())
