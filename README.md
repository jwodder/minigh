[![Project Status: WIP – Initial development is in progress, but there has not yet been a stable, usable release suitable for the public.](https://www.repostatus.org/badges/latest/wip.svg)](https://www.repostatus.org/#wip)
[![CI Status](https://github.com/jwodder/minigh/actions/workflows/test.yml/badge.svg)](https://github.com/jwodder/minigh/actions/workflows/test.yml)
[![Minimum Supported Rust Version](https://img.shields.io/badge/MSRV-1.82-orange)](https://www.rust-lang.org)
[![MIT License](https://img.shields.io/github/license/jwodder/minigh.svg)](https://opensource.org/licenses/MIT)

[GitHub](https://github.com/jwodder/minigh) | [Issues](https://github.com/jwodder/minigh/issues)

`minigh` is a thin wrapper around [`ureq`](https://crates.io/crates/ureq) for
interacting with the GitHub REST API that aims to make common operations easier
& simpler.  Notable features include:

- When making a request, you only need to specify the part of the URL after the
  API base URL.

- Support for iterating over paginated results

- Most request methods return decoded JSON.

- Bring Your Own Schema: `minigh` does not define any types for values returned
  by the API; that is left up to the user.

- Errors raised for 4xx and 5xx responses include the body of the response in
  the error value, and this body is included when displaying with `{:#}`.

- The `Accept` and `X-GitHub-Api-Version` headers are automatically set to
  their recommended values.

- Follows [GitHub's recommendations for dealing with rate limits][ratelimit],
  including waiting between mutating requests and waiting & retrying in
  response to rate-limit errors

- Automatic retrying on 5xx errors with exponential backoff

[ratelimit]: https://docs.github.com/en/rest/guides/best-practices-for-using-the-rest-api?apiVersion=2022-11-28#dealing-with-rate-limits
