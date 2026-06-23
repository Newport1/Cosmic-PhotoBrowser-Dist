# PhotoBrowser release cleanup design

## Scope

Rename the application consistently to PhotoBrowser and apply all P0/P1 cleanup
items from the release review. The repository must contain no references to the
former brand after the change.

## Identity migration

The Cargo package, library, binary, application title, application ID,
configuration/cache/export locations, cache key, thread names, test and benchmark
environment variables, documentation, and command examples will use
`photobrowser` or `PhotoBrowser` as appropriate.

The namespace migration intentionally creates new PhotoBrowser runtime locations;
it does not retain aliases to the former name.

## Release cleanup

Fixture-dependent tests will require explicit environment variables and will not
contain local paths. Public comments and docs will describe behavior rather than
development phases or internal process. Cargo metadata will identify the public
README and repository, and the specified production unwraps will be removed.

## Documentation and verification

The public README, changelog, and architecture guide will use standard filenames.
The obsolete review artifact and old generated document names will be removed.

Validation will run formatting, tests, Clippy, a no-default-features build, and a
case-insensitive repository scan for the former name.
