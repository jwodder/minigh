use super::{Method, RequestError, StatusError};
use mime::{Mime, JSON};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use ureq::{
    http::{
        header::{HeaderName, CONTENT_TYPE, LINK, RETRY_AFTER},
        response::{Parts, Response},
        status::StatusCode,
    },
    Body,
};
use url::Url;

// Retry configuration:
const RETRIES: i32 = 10;
const BACKOFF_FACTOR: f64 = 1.0;
const BACKOFF_BASE: f64 = 1.25;
const BACKOFF_MAX: f64 = 120.0;
const TOTAL_WAIT: Duration = Duration::from_secs(300);

const RATELIMIT_REMAINING_HEADER: HeaderName = HeaderName::from_static("x-ratelimit-remaining");
const RATELIMIT_RESET_HEADER: HeaderName = HeaderName::from_static("x-ratelimit-reset");

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Retrier {
    method: Method,
    url: Url,
    attempts: i32,
    stop_time: Instant,
}

impl Retrier {
    pub(super) fn new(method: Method, url: Url) -> Retrier {
        Retrier {
            method,
            url,
            attempts: 0,
            stop_time: Instant::now() + TOTAL_WAIT,
        }
    }

    // Takes the return value of a call to `Request::call()` or similar.
    //
    // - If the request was successful (status code and everything), returns
    //   `Ok(RetryDecision::Success(response))`.
    //
    // - If the request should be retried, returns
    //   `Ok(RetryDecision::Retry(delay))`.
    //
    // - If the request was a failure (possibly due to status code) and should
    //   not be retried (possibly due to all retries having been exhausted),
    //   returns an `Err`.
    pub(super) fn handle(
        &mut self,
        resp: Result<Response<Body>, ureq::Error>,
    ) -> Result<RetryDecision, RequestError> {
        self.attempts += 1;
        if self.attempts > RETRIES {
            log::debug!("Retries exhausted");
            return self.finalize(resp);
        }
        let now = Instant::now();
        let time_left = self.stop_time.saturating_duration_since(now);
        if time_left == Duration::ZERO {
            log::debug!("Maximum total retry wait time exceeded");
            return self.finalize(resp);
        }
        let backoff = if self.attempts < 2 {
            // urllib3 says "most errors are resolved immediately by a second
            // try without a delay" and thus doesn't sleep on the first retry,
            // but that seems irresponsible
            BACKOFF_FACTOR * 0.1
        } else {
            (BACKOFF_FACTOR * BACKOFF_BASE.powi(self.attempts - 1)).clamp(0.0, BACKOFF_MAX)
        };
        let backoff = Duration::from_secs_f64(backoff);
        let delay = match resp {
            Ok(r) if r.status() == StatusCode::FORBIDDEN => {
                let mut rr = ReadableResponse::new(self.method, self.url.clone(), r);
                if let Some(v) = rr.header(RETRY_AFTER) {
                    let secs = v.parse::<u64>().ok().map(|n| n + 1);
                    if let Some(delay) = secs {
                        log::debug!("Server responded with 403 and Retry-After header");
                        if time_left < Duration::from_secs(delay) {
                            log::debug!("Retrying after Retry-After would exceed maximum total retry wait time; not retrying");
                            return Err(RequestError::Status(StatusError::from(rr)));
                        }
                    }
                    Duration::from_secs(secs.unwrap_or_default())
                } else if rr.body().is_some_and(|s| s.contains("rate limit")) {
                    if rr
                        .header(RATELIMIT_REMAINING_HEADER)
                        .is_some_and(|v| v == "0")
                    {
                        if let Some(reset) = rr
                            .header(RATELIMIT_RESET_HEADER)
                            .and_then(|s| s.parse::<u64>().ok())
                        {
                            let delay = time_till_timestamp(reset).unwrap_or_default()
                                + Duration::from_secs(1);
                            if time_left < delay {
                                log::debug!("Primary rate limit exceeded; waiting for reset would exceed maximum total retry wait time; not retrying");
                                return Err(RequestError::Status(StatusError::from(rr)));
                            } else {
                                log::debug!("Primary rate limit exceeded; waiting for reset");
                            }
                            delay
                        } else {
                            Duration::ZERO
                        }
                    } else {
                        log::debug!("Secondary rate limit triggered");
                        backoff
                    }
                } else {
                    return Err(RequestError::Status(StatusError::from(rr)));
                }
            }
            Ok(r) if r.status().is_server_error() => backoff,
            Ok(ref r) if r.status().is_client_error() => return self.finalize(resp),
            Err(_) => backoff,
            Ok(_) => return self.finalize(resp),
        };
        Ok(RetryDecision::Retry(delay.min(time_left)))
    }

    fn finalize(
        &self,
        resp: Result<Response<Body>, ureq::Error>,
    ) -> Result<RetryDecision, RequestError> {
        match resp {
            Ok(r) if r.status().is_client_error() || r.status().is_server_error() => {
                Err(RequestError::Status(StatusError::from(
                    ReadableResponse::new(self.method, self.url.clone(), r),
                )))
            }
            Ok(r) => Ok(RetryDecision::Success(r)),
            Err(source) => Err(RequestError::Send {
                method: self.method,
                url: self.url.clone(),
                source: Box::new(source),
            }),
        }
    }
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub(super) enum RetryDecision {
    Success(Response<Body>),
    Retry(Duration),
}

#[derive(Debug)]
pub(super) struct ReadableResponse {
    method: Method,
    url: Url,
    parts: Parts,
    body: ReadableBody,
}

impl ReadableResponse {
    fn new(method: Method, url: Url, resp: Response<Body>) -> Self {
        let (parts, body) = resp.into_parts();
        ReadableResponse {
            method,
            url,
            parts,
            body: ReadableBody::Unread(body),
        }
    }

    fn header(&self, key: HeaderName) -> Option<&str> {
        let v = self.parts.headers.get(&key)?;
        v.to_str().ok()
    }

    fn body(&mut self) -> Option<&str> {
        self.body.as_str()
    }

    fn pretty_body(&mut self) -> Option<String> {
        if self.header(CONTENT_TYPE).is_some_and(is_json_content_type) {
            self.body
                .as_str()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .map(|v| {
                    serde_json::to_string_pretty(&v)
                        .expect("Re-JSONifying a JSON response should not fail")
                })
        } else {
            self.body
                .as_str()
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
        }
    }
}

impl From<ReadableResponse> for StatusError {
    fn from(mut value: ReadableResponse) -> StatusError {
        let body = value.pretty_body();
        StatusError {
            method: value.method,
            url: value.url,
            status: value.parts.status,
            body,
        }
    }
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
enum ReadableBody {
    Unread(Body),
    Read(Option<String>),
}

impl ReadableBody {
    fn as_str(&mut self) -> Option<&str> {
        if let ReadableBody::Unread(ref mut body) = self {
            *self = ReadableBody::Read(body.read_to_string().ok());
        }
        let ReadableBody::Read(ref s) = self else {
            unreachable!("ReadableBody should be Read after reading");
        };
        s.as_deref()
    }
}

/// Return the `rel="next"` URL, if any, from the response's "Link" header
pub(super) fn get_next_link(r: &Response<Body>) -> Option<Url> {
    let header_value = r.headers().get(LINK)?.to_str().ok()?;
    parse_link_header::parse_with_rel(header_value)
        .ok()?
        .get("next")
        .map(|link| link.uri.clone())
}

fn is_json_content_type(ct_value: &str) -> bool {
    ct_value.parse::<Mime>().ok().is_some_and(|ct| {
        ct.type_() == "application" && (ct.subtype() == "json" || ct.suffix() == Some(JSON))
    })
}

fn time_till_timestamp(ts: u64) -> Option<Duration> {
    (UNIX_EPOCH + Duration::from_secs(ts))
        .duration_since(SystemTime::now())
        .ok()
}
