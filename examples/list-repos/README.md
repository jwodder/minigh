This is an example program that uses `minigh` to list all of the public GitHub
repositories for a given GitHub user or organization.  In order to run the
program, you must have a GitHub access token set via the `GH_TOKEN` or
`GITHUB_TOKEN` environment variable or else saved with
[`gh`](https://github.com/cli/cli).

Usage
=====

    cargo run -- [-J|--json] <owner>

Fetch all of the public GitHub repositories for `<owner>` and display various
information about them.  If the `-J`/`--json` option is given, the output is
formatted as a series of JSON objects.
