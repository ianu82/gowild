# Documentation boundary

GoWild documentation has two deliberately separate classes.

## Active GoWild documentation

The top-level files directly under [`docs/next`](next/README.md) describe the
current unreleased GoWild product. These files may be updated with product work
and are the only documentation source that may become a future GoWild release.

## Frozen imported records

These paths are retained only as read-only records of the imported source
snapshot:

- `docs/next/website/`
- `docs/preview/`
- `docs/versions/`
- `website/`, except for installer code still exercised by package tests

Their pages, brands, links, commands, changelogs, release numbers, and
translations are historical. They are not GoWild product claims, are not
maintained GoWild documentation, and must not be served, indexed, packaged, or
published. Keeping those files unchanged preserves an auditable import and
avoids presenting a mechanical rename as new authorship.

The repository's website and release recipes fail closed until GoWild has its
own reviewed site, artifacts, signing, manifests, and installation path. No
automation may sync these records from or publish them to the source-provenance
project.

See [`ACKNOWLEDGEMENTS`](../ACKNOWLEDGEMENTS/README.md) for the exact imported
commit and legal attribution.
