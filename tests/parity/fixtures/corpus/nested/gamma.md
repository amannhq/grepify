# Gamma document (nested)

This file lives in a subdirectory to exercise recursive walking. The relative
path (`nested/gamma.md`) becomes part of the stable component key in both hosts.

## Body

When we edit exactly one source file and re-run, only the component that owns
that file should reprocess. Every other component is a memo hit and stays
unchanged. This nested file is the one the incremental test edits.
