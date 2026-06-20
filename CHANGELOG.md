v0.3.0 (2026-06-20)
-------------------
- Updated the default value of the `X-GitHub-Api-Version` header to
  "2026-03-10", up from "2022-11-28".  See [the GitHub
  documentation](https://docs.github.com/en/rest/about-the-rest-api/breaking-changes#version-2026-03-10)
  for information on breaking changes in the new API version.

v0.2.0 (2025-11-14)
-------------------
- Increased MSRV to 1.88
- Added a `ClientBuilder` for setting a client's access token, user agent, base
  API URL, API version header value, and/or "Accept" header value
- Added a `Client::agent_ref()` method

v0.1.1 (2025-06-27)
-------------------
- The `Display` impl for `Method` now supports width, fill, alignment, and
  precision flags

v0.1.0 (2025-06-08)
-------------------
Initial release
