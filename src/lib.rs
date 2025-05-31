mod util;
pub use crate::util::*;
use serde::{de::DeserializeOwned, Serialize};
use std::cell::Cell;
use std::thread::sleep;
use std::time::{Duration, Instant};
use thiserror::Error;
use ureq::{Agent, AgentBuilder, Response};
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

const MUTATION_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone, Debug)]
pub struct GitHub {
    client: Agent,
    api_url: Url,
    last_mutation: Cell<Option<Instant>>,
}

impl GitHub {
    pub fn new(token: &str) -> GitHub {
        let Ok(api_url) = Url::parse(GITHUB_API_URL) else {
            unreachable!("GITHUB_API_URL should be a valid URL");
        };
        let auth = format!("Bearer {token}");
        let client = AgentBuilder::new()
            .user_agent(USER_AGENT)
            .https_only(true)
            .middleware(move |req: ureq::Request, next: ureq::MiddlewareNext<'_>| {
                next.handle(
                    req.set("Authorization", &auth)
                        .set("Accept", "application/vnd.github+json")
                        .set("X-GitHub-Api-Version", "2022-11-28"),
                )
            })
            .build();
        GitHub {
            client,
            api_url,
            last_mutation: Cell::new(None),
        }
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
    ) -> Result<Response, RequestError> {
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
        let req = self.client.request_url(method.as_str(), &url);
        //let mut retrier = Retrier::new(method, url.clone());
        let mut retrier = Retrier::new(method, url);
        loop {
            if method.is_mutating() {
                self.last_mutation.set(Some(Instant::now()));
            }
            let req = req.clone();
            //log::trace!("{} {}", method.as_str(), url);
            let resp = if let Some(p) = payload {
                req.send_json(p)
            } else {
                req.call()
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
        let r = self.raw_request::<T>(method, url.clone(), payload)?;
        match r.into_json::<U>() {
            Ok(val) => Ok(val),
            Err(source) => Err(RequestError::Deserialize {
                method,
                url,
                source,
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

    pub fn paginate<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>, RequestError> {
        let mut items = Vec::new();
        let mut url = self.mkurl(path)?;
        loop {
            let r = self.raw_request::<()>(Method::Get, url.clone(), None)?;
            let next_url = get_next_link(&r);
            match r.into_json::<Vec<T>>() {
                Ok(page) => items.extend(page),
                Err(source) => {
                    return Err(RequestError::Deserialize {
                        method: Method::Get,
                        url,
                        source,
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
        source: Box<ureq::Transport>,
    },
    #[error(transparent)]
    Status(PrettyHttpError),
    #[error("failed to deserialize response body from {method} request to {url}")]
    Deserialize {
        method: Method,
        url: Url,
        source: std::io::Error,
    },
}
