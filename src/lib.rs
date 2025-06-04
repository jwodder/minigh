mod util;
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
        header::{HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE},
        status::StatusCode,
        Response,
    },
    Agent, Body, ResponseExt,
};
use url::Url;

static USER_AGENT: &str = concat!(
    env!("CARGO_PKG_NAME"),
    "/",
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CARGO_PKG_REPOSITORY"),
    ")",
);

static GITHUB_API_URL: &str = "https://api.github.com";

static ACCEPT_VALUE: &str = "application/vnd.github+json";

const API_VERSION_HEADER: HeaderName = HeaderName::from_static("x-github-api-version");
const API_VERSION_VALUE: HeaderValue = HeaderValue::from_static("2022-11-28");

const MUTATION_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone, Debug)]
pub struct Client {
    inner: Agent,
    api_url: Url,
    last_mutation: Cell<Option<Instant>>,
}

impl Client {
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

    fn mkurl(&self, path: &str) -> Result<Url, RequestError> {
        self.api_url
            .join(path)
            .map_err(|source| RequestError::Path {
                source,
                path: path.to_owned(),
            })
    }

    pub fn raw_request<T: Serialize>(
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
                    //log::trace!("Sleeping for {delay:?} between mutating requests");
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
            //log::trace!("{} {}", method.as_str(), url);
            let resp = if let Some(p) = payload {
                req.send_json(p)
            } else {
                req.send_empty()
            };
            /*
            let desc = match &resp {
                Ok(_) => Cow::from("Request succeeded"),
                Err(ureq::Error::Status(code, _)) => {
                    Cow::from(format!("Server returned {code} response"))
                }
                Err(e) => Cow::from(format!("Request failed: {e}")),
            };
            */
            match retrier.handle(resp)? {
                RetryDecision::Success(r) => return Ok(r),
                RetryDecision::Retry(delay) => {
                    //log::warn!("{desc}; waiting {delay:?} and retrying");
                    sleep(delay);
                }
            }
        }
    }

    pub fn request<T: Serialize, U: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        payload: Option<&T>,
    ) -> Result<U, RequestError> {
        let url = self.mkurl(path)?;
        let mut r = self.raw_request::<T>(method, url.clone(), payload)?;
        match r.body_mut().read_json::<U>() {
            Ok(val) => Ok(val),
            Err(source) => Err(RequestError::Deserialize {
                method,
                url,
                source: Box::new(source),
            }),
        }
    }

    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, RequestError> {
        self.request::<(), T>(Method::Get, path, None)
    }

    pub fn post<T: Serialize, U: DeserializeOwned>(
        &self,
        path: &str,
        payload: &T,
    ) -> Result<U, RequestError> {
        self.request::<T, U>(Method::Post, path, Some(payload))
    }

    pub fn put<T: Serialize, U: DeserializeOwned>(
        &self,
        path: &str,
        payload: &T,
    ) -> Result<U, RequestError> {
        self.request::<T, U>(Method::Put, path, Some(payload))
    }

    pub fn patch<T: Serialize, U: DeserializeOwned>(
        &self,
        path: &str,
        payload: &T,
    ) -> Result<U, RequestError> {
        self.request::<T, U>(Method::Patch, path, Some(payload))
    }

    pub fn delete(&self, path: &str) -> Result<(), RequestError> {
        let url = self.mkurl(path)?;
        self.raw_request::<()>(Method::Delete, url, None)?;
        Ok(())
    }

    pub fn paginate<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>, RequestError> {
        let mut items = Vec::new();
        let mut url = self.mkurl(path)?;
        loop {
            let mut r = self.raw_request::<()>(Method::Get, url.clone(), None)?;
            let next_url = get_next_link(&r);
            match r.body_mut().read_json::<Vec<T>>() {
                Ok(page) => items.extend(page),
                Err(source) => {
                    return Err(RequestError::Deserialize {
                        method: Method::Get,
                        url,
                        source: Box::new(source),
                    })
                }
            }
            match next_url {
                Some(u) => url = u,
                None => return Ok(items),
            }
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Method {
    Get,
    Post,
    Patch,
    Put,
    Delete,
}

impl Method {
    fn is_mutating(&self) -> bool {
        matches!(
            self,
            Method::Post | Method::Patch | Method::Put | Method::Delete
        )
    }

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

#[derive(Debug, Error)]
pub enum BuildClientError {
    #[error("could not create an HTTP header value out of auth token")]
    AuthValue(#[from] ureq::http::header::InvalidHeaderValue),
}

#[derive(Debug, Error)]
pub enum RequestError {
    #[error("failed to construct a GitHub API URL from path {path:?}")]
    Path {
        source: url::ParseError,
        path: String,
    },
    #[error("failed to make {method} request to {url}")]
    Send {
        method: Method,
        url: Url,
        source: Box<ureq::Error>,
    },
    #[error(transparent)]
    Status(StatusError),
    #[error("failed to deserialize response body from {method} request to {url}")]
    Deserialize {
        method: Method,
        url: Url,
        source: Box<ureq::Error>,
    },
}

impl RequestError {
    pub fn body(&self) -> Option<&str> {
        if let RequestError::Status(ref stat) = self {
            stat.body()
        } else {
            None
        }
    }
}

/// Error raised for a 4xx or 5xx HTTP response that includes the response body
/// — and, if that body is JSON, it's pretty-printed
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusError {
    method: Method,
    url: String,
    status: StatusCode,
    body: Option<String>,
}

impl StatusError {
    fn new(method: Method, mut r: Response<Body>) -> StatusError {
        let url = r.get_uri().to_string();
        let status = r.status();
        // If the response body is JSON, pretty-print it.
        let body = if is_json_response(&r) {
            r.body_mut().read_json::<serde_json::Value>().ok().map(|v| {
                serde_json::to_string_pretty(&v)
                    .expect("Re-JSONifying a JSON response should not fail")
            })
        } else {
            r.body_mut().read_to_string().ok()
        };
        StatusError {
            method,
            url,
            status,
            body: body.filter(|s| !s.is_empty()),
        }
    }

    fn from_parts(method: Method, url: Url, parts: ResponseParts) -> StatusError {
        let status = parts.status;
        // If the response body is JSON, pretty-print it.
        let body = if parts.header(CONTENT_TYPE).is_some_and(is_json_content_type) {
            parts
                .text
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .map(|v| {
                    serde_json::to_string_pretty(&v)
                        .expect("Re-JSONifying a JSON response should not fail")
                })
        } else {
            parts.text
        };
        StatusError {
            method,
            url: url.to_string(),
            status,
            body: body.filter(|s| !s.is_empty()),
        }
    }

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
