use super::{Method, RequestError, StatusError};
use mime::{Mime, JSON};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use ureq::{
    http::{
        header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE, LINK, RETRY_AFTER},
        status::StatusCode,
        Response,
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
            //log::trace!("Retries exhausted");
            return self.finalize(resp);
        }
        let now = Instant::now();
        if now > self.stop_time {
            //log::trace!("Maximum total retry wait time exceeded");
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
                let parts = ResponseParts::from_response(r);
                if let Some(v) = parts.header(RETRY_AFTER) {
                    let secs = v.parse::<u64>().ok().map(|n| n + 1);
                    /*
                    if secs.is_some() {
                        log::trace!("Server responded with 403 and Retry-After header");
                    }
                    */
                    Duration::from_secs(secs.unwrap_or_default())
                } else if parts
                    .text
                    .as_ref()
                    .is_some_and(|s| s.contains("rate limit"))
                {
                    if parts
                        .header(RATELIMIT_REMAINING_HEADER)
                        .is_some_and(|v| v == "0")
                    {
                        if let Some(reset) = parts
                            .header(RATELIMIT_RESET_HEADER)
                            .and_then(|s| s.parse::<u64>().ok())
                        {
                            //log::trace!("Primary rate limit exceeded; waiting for reset");
                            time_till_timestamp(reset).unwrap_or_default() + Duration::from_secs(1)
                        } else {
                            Duration::ZERO
                        }
                    } else {
                        //log::trace!("Secondary rate limit triggered");
                        backoff
                    }
                } else {
                    return self.finalize_parts(parts);
                }
            }
            Ok(r) if r.status().is_server_error() => backoff,
            Ok(ref r) if r.status().is_client_error() => return self.finalize(resp),
            Err(_) => backoff,
            Ok(_) => return self.finalize(resp),
        };
        let delay = delay.max(backoff);
        let time_left = self.stop_time.saturating_duration_since(Instant::now());
        Ok(RetryDecision::Retry(delay.clamp(Duration::ZERO, time_left)))
    }

    fn finalize(
        &self,
        resp: Result<Response<Body>, ureq::Error>,
    ) -> Result<RetryDecision, RequestError> {
        match resp {
            Ok(r) if r.status().is_client_error() || r.status().is_server_error() => Err(
                RequestError::Status(StatusError::from_response(self.method, self.url.clone(), r)),
            ),
            Ok(r) => Ok(RetryDecision::Success(r)),
            Err(source) => Err(RequestError::Send {
                method: self.method,
                url: self.url.clone(),
                source: Box::new(source),
            }),
        }
    }

    fn finalize_parts<T>(&self, parts: ResponseParts) -> Result<T, RequestError> {
        Err(RequestError::Status(StatusError::from_parts(
            self.method,
            self.url.clone(),
            parts,
        )))
    }
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub(super) enum RetryDecision {
    Success(Response<Body>),
    Retry(Duration),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ResponseParts {
    pub(super) status: StatusCode,
    pub(super) headers: HeaderMap<HeaderValue>,
    pub(super) text: Option<String>,
}

impl ResponseParts {
    fn from_response(r: Response<Body>) -> ResponseParts {
        let (parts, mut body) = r.into_parts();
        let status = parts.status;
        let headers = parts.headers;
        let text = body.read_to_string().ok();
        ResponseParts {
            status,
            headers,
            text,
        }
    }

    pub(super) fn header(&self, key: HeaderName) -> Option<&str> {
        let v = self.headers.get(&key)?;
        v.to_str().ok()
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

/// Returns `true` iff the response's Content-Type header indicates the body is
/// JSON
pub(super) fn is_json_response(r: &Response<Body>) -> bool {
    r.headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(is_json_content_type)
}

pub(super) fn is_json_content_type(ct_value: &str) -> bool {
    ct_value.parse::<Mime>().ok().is_some_and(|ct| {
        ct.type_() == "application" && (ct.subtype() == "json" || ct.suffix() == Some(JSON))
    })
}

fn time_till_timestamp(ts: u64) -> Option<Duration> {
    (UNIX_EPOCH + Duration::from_secs(ts))
        .duration_since(SystemTime::now())
        .ok()
}
