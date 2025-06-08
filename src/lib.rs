//! `minigh` is a thin wrapper around [`ureq`] for interacting with the GitHub
//! REST API that aims to make common operations easier & simpler.  Notable
//! features include:
//!
//! - When making a request, you only need to specify the part of the URL after
//!   the API base URL.
//!
//! - Support for iterating over paginated results
//!
//! - Most request methods return decoded JSON.
//!
//! - Bring Your Own Schema: `minigh` does not define any types for values
//!   returned by the API; that is left up to the user.
//!
//! - Errors raised for 4xx and 5xx responses include the body of the response
//!   in the error value, and this body is included when displaying with `{:#}`.
//!
//! - The `Accept` and `X-GitHub-Api-Version` headers are automatically set to
//!   their recommended values.
//!
//! - Follows [GitHub's recommendations for dealing with rate
//!   limits][ratelimit], including waiting between mutating requests and
//!   waiting & retrying in response to rate-limit errors
//!
//! - Automatic retrying on 5xx errors with exponential backoff
//!
//! [ratelimit]: https://docs.github.com/en/rest/guides/best-practices-for-using-the-rest-api?apiVersion=2022-11-28#dealing-with-rate-limits
//!
//! Logging
//! =======
//!
//! `minigh` uses the [`log`] crate to log events.  All messages are currently
//! logged at the `DEBUG` level.
mod page;
mod util;
pub use crate::page::*;
use crate::util::*;
use indenter::indented;
use serde::{de::DeserializeOwned, Serialize};
use std::cell::Cell;
use std::fmt::{self, Write};
use std::thread::sleep;
use std::time::{Duration, Instant};
use thiserror::Error;
use ureq::{
    http::{
        header::{HeaderName, HeaderValue, AUTHORIZATION},
        status::StatusCode,
        Response,
    },
    Agent, Body,
};
use url::Url;

/// The `User-Agent` header sent in requests
static USER_AGENT: &str = concat!(
    env!("CARGO_PKG_NAME"),
    "/",
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CARGO_PKG_REPOSITORY"),
    ")",
);

/// The base GitHub REST API URL
static GITHUB_API_URL: &str = "https://api.github.com";

/// The value of the `Accept` header sent in requests
static ACCEPT_VALUE: &str = "application/vnd.github+json";

/// The name of the `X-GitHub-Api-Version` header
const API_VERSION_HEADER: HeaderName = HeaderName::from_static("x-github-api-version");

/// The value of the `X-GitHub-Api-Version` header sent in requests
const API_VERSION_VALUE: HeaderValue = HeaderValue::from_static("2022-11-28");

/// Delay between consecutive requests that use mutating methods
const MUTATION_DELAY: Duration = Duration::from_secs(1);

/// A client for the GitHub REST API
#[derive(Clone, Debug)]
pub struct Client {
    /// The inner [`ureq::Agent`]
    inner: Agent,

    /// The base API URL
    api_url: Url,

    /// The timestamp of the most recent request, if any, made with this client
    /// that used a mutating method
    last_mutation: Cell<Option<Instant>>,
}

impl Client {
    /// Create a new `Client` using the given authentication token.
    ///
    /// All requests made with the client will send the following headers:
    ///
    /// - `Accept: application/vnd.github+json`
    /// - `Authorization: Bearer {token}` (included in redirects to the same
    ///   host)
    /// - `User-Agent` — value constructed from `minigh` package details
    /// - `X-GitHub-Api-Version: 2022-11-28`
    ///
    /// In addition, only HTTPS requests (including redirects) are supported by
    /// the client.
    ///
    /// # Errors
    ///
    /// Returns `Err` if `"Bearer {token}"` is not a valid HTTP header value
    pub fn new(token: &str) -> Result<Client, BuildClientError> {
        let Ok(api_url) = Url::parse(GITHUB_API_URL) else {
            unreachable!("GITHUB_API_URL should be a valid URL");
        };
        let auth = format!("Bearer {token}");
        let auth = HeaderValue::from_str(&auth)?;
        let inner = Agent::config_builder()
            .http_status_as_error(false)
            .redirect_auth_headers(ureq::config::RedirectAuthHeaders::SameHost)
            .user_agent(USER_AGENT)
            .accept(ACCEPT_VALUE)
            .https_only(true)
            .middleware(
                move |mut req: ureq::http::Request<ureq::SendBody<'_>>,
                      next: ureq::middleware::MiddlewareNext<'_>| {
                    req.headers_mut().insert(AUTHORIZATION, auth.clone());
                    req.headers_mut()
                        .insert(API_VERSION_HEADER, API_VERSION_VALUE);
                    next.handle(req)
                },
            )
            .build()
            .into();
        Ok(Client {
            inner,
            api_url,
            last_mutation: Cell::new(None),
        })
    }

    /// If `path` is a URL, return it as-is.  Otherwise, return it joined to
    /// `self.api_url`.
    fn mkurl(&self, path: &str) -> Result<Url, RequestError> {
        self.api_url
            .join(path)
            .map_err(|source| RequestError::Path {
                source,
                path: path.to_owned(),
            })
    }

    /// Make an HTTP request with method `method` to URL `url`.  If `payload`
    /// is not `None`, it is serialized as JSON and sent as the request body.
    /// Returns an [`ureq::http::Response`] with a [`ureq::Body`] body.
    ///
    /// If `method` is a mutating method (POST, PATCH, PUT, or DELETE), sleep
    /// until at least one second has passed since the most recent request with
    /// a mutating method was made.
    ///
    /// If the request fails for any of the following reasons:
    ///
    /// - a low-level I/O error occurs (e.g., connection or HTTPS failure)
    ///
    /// - a 403 response is returned, and either the `Retry-After` header is
    ///   present or the body contains the string `"rate limit"`
    ///
    /// - the server responds with a 5xx status code
    ///
    /// then the method sleeps for a bit and retries the response.  If ten
    /// retries occur or five minutes elapse and the request is still failing,
    /// `Err` is returned.  The sleep duration is computed based on
    /// the `Retry-After` header, the `X-RateLimit-Reset` header, or
    /// exponential backoff, as appropriate.
    pub fn request<T: Serialize>(
        &self,
        method: Method,
        url: Url,
        payload: Option<&T>,
    ) -> Result<Response<Body>, RequestError> {
        if method.is_mutating() {
            if let Some(lastmut) = self.last_mutation.get() {
                let delay = MUTATION_DELAY
                    .saturating_sub(Instant::now().saturating_duration_since(lastmut));
                if !delay.is_zero() {
                    log::debug!("Sleeping for {delay:?} between mutating requests");
                    sleep(delay);
                }
            }
        }
        let mut retrier = Retrier::new(method, url.clone());
        loop {
            if method.is_mutating() {
                self.last_mutation.set(Some(Instant::now()));
            }
            let req = match method {
                Method::Get => self.inner.get(url.as_str()).force_send_body(),
                //Method::Head => self.inner.head(url).force_send_body(),
                Method::Post => self.inner.post(url.as_str()),
                Method::Put => self.inner.put(url.as_str()),
                Method::Patch => self.inner.patch(url.as_str()),
                Method::Delete => self.inner.delete(url.as_str()).force_send_body(),
            };
            log::debug!("{method} {url}");
            let resp = if let Some(p) = payload {
                req.send_json(p)
            } else {
                req.send_empty()
            };
            match &resp {
                Ok(r) => log::debug!("Server returned {}", r.status()),
                Err(e) => log::debug!("Request failed: {e}"),
            };
            match retrier.handle(resp)? {
                RetryDecision::Success(r) => return Ok(r),
                RetryDecision::Retry(delay) => {
                    log::debug!("Waiting {delay:?} and then retrying request");
                    sleep(delay);
                }
            }
        }
    }

    /// Make an HTTP request with method `method` to `path`.  `path` may be
    /// either a complete URL or a URL path to append to the base GitHub API
    /// URL (e.g., `"/users/octocat/repos"`).
    ///
    /// If `payload` is not `None`, it is serialized as JSON and sent as the
    /// request body.
    ///
    /// Deserializes the response body as `U` and returns the result.
    ///
    /// See [`request()`][Client::request] for information on lower-level
    /// behavior.
    pub fn request_json<T: Serialize, U: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        payload: Option<&T>,
    ) -> Result<U, RequestError> {
        let url = self.mkurl(path)?;
        let mut r = self.request::<T>(method, url.clone(), payload)?;
        match r.body_mut().read_json::<U>() {
            Ok(val) => Ok(val),
            Err(source) => Err(RequestError::Deserialize {
                method,
                url,
                source: Box::new(source),
            }),
        }
    }

    /// Make a GET request to `path`.  `path` may be either a complete URL or
    /// a URL path to append to the base GitHub API URL (e.g.,
    /// `"/users/octocat/repos"`).
    ///
    /// Deserializes the response body as `T` and returns the result.
    ///
    /// See [`request()`][Client::request] for information on lower-level
    /// behavior.
    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, RequestError> {
        self.request_json::<(), T>(Method::Get, path, None)
    }

    /// Make a POST request to `path`.  `path` may be either a complete URL or
    /// a URL path to append to the base GitHub API URL (e.g.,
    /// `"/users/octocat/repos"`).
    ///
    /// `payload` is serialized as JSON and sent as the request body.
    ///
    /// Deserializes the response body as `U` and returns the result.
    ///
    /// See [`request()`][Client::request] for information on lower-level
    /// behavior.
    pub fn post<T: Serialize, U: DeserializeOwned>(
        &self,
        path: &str,
        payload: &T,
    ) -> Result<U, RequestError> {
        self.request_json::<T, U>(Method::Post, path, Some(payload))
    }

    /// Make a PUT request to `path`.  `path` may be either a complete URL or
    /// a URL path to append to the base GitHub API URL (e.g.,
    /// `"/users/octocat/repos"`).
    ///
    /// `payload` is serialized as JSON and sent as the request body.
    ///
    /// Deserializes the response body as `U` and returns the result.
    ///
    /// See [`request()`][Client::request] for information on lower-level
    /// behavior.
    pub fn put<T: Serialize, U: DeserializeOwned>(
        &self,
        path: &str,
        payload: &T,
    ) -> Result<U, RequestError> {
        self.request_json::<T, U>(Method::Put, path, Some(payload))
    }

    /// Make a PATCH request to `path`.  `path` may be either a complete URL or
    /// a URL path to append to the base GitHub API URL (e.g.,
    /// `"/users/octocat/repos"`).
    ///
    /// `payload` is serialized as JSON and sent as the request body.
    ///
    /// Deserializes the response body as `U` and returns the result.
    ///
    /// See [`request()`][Client::request] for information on lower-level
    /// behavior.
    pub fn patch<T: Serialize, U: DeserializeOwned>(
        &self,
        path: &str,
        payload: &T,
    ) -> Result<U, RequestError> {
        self.request_json::<T, U>(Method::Patch, path, Some(payload))
    }

    /// Make a DELETE request to `path`.  `path` may be either a complete URL
    /// or a URL path to append to the base GitHub API URL (e.g.,
    /// `"/users/octocat/repos"`).
    ///
    /// See [`request()`][Client::request] for information on lower-level
    /// behavior.
    pub fn delete(&self, path: &str) -> Result<(), RequestError> {
        let url = self.mkurl(path)?;
        self.request::<()>(Method::Delete, url, None)?;
        Ok(())
    }

    /// Returns an iterator that makes a paginated series of GET requests, starting
    /// with a request to `path` and continuing with the URLs specified in the
    /// "next" relations of the `Link` response headers, and yields the resulting
    /// items of type `T` as they are fetched.  Both responses consisting of an
    /// array of `T` and a map containing an array field of item type `T` are
    /// supported.
    ///
    /// `path` may be either a complete URL or a URL path to append to the base
    /// GitHub API URL (e.g., `"/users/octocat/repos"`).
    ///
    /// See [`request()`][Client::request] for information on lower-level
    /// behavior.
    pub fn paginate<T: DeserializeOwned>(&self, path: &str) -> PaginationIter<'_, T> {
        PaginationIter::new(self, path)
    }
}

/// The HTTP methods supported by `minigh`
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Method {
    Get,
    Post,
    Patch,
    Put,
    Delete,
}

impl Method {
    /// Returns `true` if the method is a mutating method (POST, PATCH, PUT, or
    /// DELETE)
    pub fn is_mutating(&self) -> bool {
        matches!(
            self,
            Method::Post | Method::Patch | Method::Put | Method::Delete
        )
    }

    /// Returns the name of the method as an uppercase string
    pub fn as_str(&self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when constructing a `Client` fails
#[derive(Debug, Error)]
pub enum BuildClientError {
    /// Failed to create a valid HTTP header value out of the supplied
    /// authentication token
    #[error("could not create an HTTP header value out of auth token")]
    AuthValue(#[from] ureq::http::header::InvalidHeaderValue),
}

/// Error returned when an HTTP request fails
#[derive(Debug, Error)]
pub enum RequestError {
    /// Failed to construct a valid URL from a given path
    #[error("failed to construct a GitHub API URL from path {path:?}")]
    Path {
        /// The inner [`url::ParseError`]
        source: url::ParseError,

        /// The supplied `path` value
        path: String,
    },

    /// Failed to perform the HTTP request
    #[error("failed to make {method} request to {url}")]
    Send {
        /// The HTTP method of the attempted request
        method: Method,

        /// The URL to which the request was sent
        url: Url,

        /// The inner [`ureq::Error`]
        source: Box<ureq::Error>,
    },

    /// The server returned a 4xx or 5xx status code
    #[error(transparent)]
    Status(StatusError),

    /// Failed to deserialize the response body as JSON
    #[error("failed to deserialize response body from {method} request to {url}")]
    Deserialize {
        /// The HTTP method of the attempted request
        method: Method,

        /// The URL to which the request was sent
        url: Url,

        /// The inner [`ureq::Error`]
        source: Box<ureq::Error>,
    },
}

impl RequestError {
    /// If the request failed due to a 4xx or 5xx response, and a nonempty
    /// response body was read, return the body.  If the response's headers
    /// indicated the body was JSON, the body is pretty-printed.
    ///
    /// The body is also printed when displaying a `RequestError` with `{:#}`.
    pub fn body(&self) -> Option<&str> {
        if let RequestError::Status(ref stat) = self {
            stat.body()
        } else {
            None
        }
    }
}

/// Error returned when the server replies with a 4xx or 5xx status code
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusError {
    /// The HTTP method of the attempted request
    pub method: Method,

    /// The URL to which the request was sent
    pub url: Url,

    /// The response's status code
    pub status: StatusCode,

    /// The response body, if read successfully and nonempty.  If the
    /// response's headers indicated the body was JSON, it is pretty-printed.
    pub body: Option<String>,
}

impl StatusError {
    /// If a nonempty response body was read, return the body.  If the
    /// response's headers indicated the body was JSON, the body is
    /// pretty-printed.
    ///
    /// The body is also printed when displaying a `StatusError` with `{:#}`.
    pub fn body(&self) -> Option<&str> {
        self.body.as_deref()
    }
}

impl fmt::Display for StatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} request to {} returned {}",
            self.method, self.url, self.status
        )?;
        if f.alternate() {
            if let Some(text) = self.body() {
                write!(indented(f).with_str("    "), "\n\n{text}\n")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for StatusError {}
