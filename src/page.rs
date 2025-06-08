use super::util::get_next_link;
use super::{Client, Method, RequestError};
use serde::{de::DeserializeOwned, Deserialize};
use std::collections::HashMap;
use thiserror::Error;
use url::Url;

#[derive(Clone, Debug)]
pub struct PaginationIter<'a, T> {
    client: &'a Client,
    next_url: Option<Url>,
    items: Option<std::vec::IntoIter<T>>,
}

impl<'a, T> PaginationIter<'a, T> {
    pub fn new(client: &'a Client, url: Url) -> Self {
        PaginationIter {
            client,
            next_url: Some(url),
            items: None,
        }
    }
}

impl<T> Iterator for PaginationIter<'_, T>
where
    T: DeserializeOwned,
{
    type Item = Result<T, RequestError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.items.as_mut().and_then(Iterator::next) {
                return Some(Ok(item));
            } else {
                self.items = None;
            }
            if let Some(url) = self.next_url.take() {
                let mut resp = match self.client.request::<()>(Method::Get, url.clone(), None) {
                    Ok(r) => r,
                    Err(e) => return Some(Err(e)),
                };
                match resp.body_mut().read_json::<Page<T>>() {
                    Ok(page) => self.items = Some(page.items.into_iter()),
                    Err(source) => {
                        return Some(Err(RequestError::Deserialize {
                            method: Method::Get,
                            url,
                            source: Box::new(source),
                        }))
                    }
                }
                self.next_url = get_next_link(&resp);
            } else {
                return None;
            }
        }
    }
}

impl<T> std::iter::FusedIterator for PaginationIter<'_, T> where T: DeserializeOwned {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(bound = "T: DeserializeOwned", try_from = "RawPage<T>")]
struct Page<T> {
    items: Vec<T>,
    total_count: Option<u64>,
    incomplete_results: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
enum RawPage<T> {
    Array(Vec<T>),
    Map(HashMap<String, MapPageValue<T>>),
}

impl<T: DeserializeOwned> TryFrom<RawPage<T>> for Page<T> {
    type Error = ParsePageError;

    fn try_from(value: RawPage<T>) -> Result<Page<T>, ParsePageError> {
        match value {
            RawPage::Array(items) => Ok(Page {
                items,
                total_count: None,
                incomplete_results: None,
            }),
            RawPage::Map(map) => {
                let total_count = map.get("total_count").and_then(MapPageValue::as_u64);
                let incomplete_results = map
                    .get("incomplete_results")
                    .and_then(MapPageValue::as_bool);
                let mut lists = map
                    .into_values()
                    .filter_map(MapPageValue::into_list)
                    .collect::<Vec<_>>();
                if lists.len() == 1 {
                    let Some(items) = lists.pop() else {
                        unreachable!("Vec with 1 item should have something to pop");
                    };
                    Ok(Page {
                        items,
                        total_count,
                        incomplete_results,
                    })
                } else {
                    Err(ParsePageError::ListQty(lists.len()))
                }
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
enum MapPageValue<T> {
    Count(u64),
    Bool(bool),
    List(Vec<T>),
    Other(serde::de::IgnoredAny),
}

impl<T> MapPageValue<T> {
    fn as_u64(&self) -> Option<u64> {
        if let MapPageValue::Count(value) = self {
            Some(*value)
        } else {
            None
        }
    }

    fn as_bool(&self) -> Option<bool> {
        if let MapPageValue::Bool(value) = self {
            Some(*value)
        } else {
            None
        }
    }

    fn into_list(self) -> Option<Vec<T>> {
        if let MapPageValue::List(lst) = self {
            Some(lst)
        } else {
            None
        }
    }
}

#[derive(Debug, Error)]
enum ParsePageError {
    #[error("expected exactly one array of items in map page response, got {0}")]
    ListQty(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    mod deser_page {
        use super::*;
        use indoc::indoc;

        #[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
        struct Widget {
            name: String,
            color: String,
            power: u64,
        }

        #[test]
        fn from_list() {
            let src = indoc! {r#"
            [
                {
                    "name": "Steve",
                    "color": "aquamarine",
                    "power": 9001
                },
                {
                    "name": "Widget O'Malley",
                    "color": "taupe",
                    "power": 42
                }
            ]
            "#};
            let page = serde_json::from_str::<Page<Widget>>(src).unwrap();
            assert_eq!(
                page,
                Page {
                    items: vec![
                        Widget {
                            name: "Steve".into(),
                            color: "aquamarine".into(),
                            power: 9001,
                        },
                        Widget {
                            name: "Widget O'Malley".into(),
                            color: "taupe".into(),
                            power: 42,
                        },
                    ],
                    total_count: None,
                    incomplete_results: None,
                }
            );
        }

        #[test]
        fn from_map() {
            let src = indoc! {r#"
            {
                "total_count": 17,
                "widgets": [
                    {
                        "name": "Steve",
                        "color": "aquamarine",
                        "power": 9001
                    },
                    {
                        "name": "Widget O'Malley",
                        "color": "taupe",
                        "power": 42
                    }
                ]
            }
            "#};
            let page = serde_json::from_str::<Page<Widget>>(src).unwrap();
            assert_eq!(
                page,
                Page {
                    items: vec![
                        Widget {
                            name: "Steve".into(),
                            color: "aquamarine".into(),
                            power: 9001,
                        },
                        Widget {
                            name: "Widget O'Malley".into(),
                            color: "taupe".into(),
                            power: 42,
                        },
                    ],
                    total_count: Some(17),
                    incomplete_results: None,
                }
            );
        }

        #[test]
        fn from_map_no_total() {
            let src = indoc! {r#"
            {
                "widgets": [
                    {
                        "name": "Steve",
                        "color": "aquamarine",
                        "power": 9001
                    },
                    {
                        "name": "Widget O'Malley",
                        "color": "taupe",
                        "power": 42
                    }
                ]
            }
            "#};
            let page = serde_json::from_str::<Page<Widget>>(src).unwrap();
            assert_eq!(
                page,
                Page {
                    items: vec![
                        Widget {
                            name: "Steve".into(),
                            color: "aquamarine".into(),
                            power: 9001,
                        },
                        Widget {
                            name: "Widget O'Malley".into(),
                            color: "taupe".into(),
                            power: 42,
                        },
                    ],
                    total_count: None,
                    incomplete_results: None,
                }
            );
        }

        #[test]
        fn from_map_extra_field() {
            let src = indoc! {r#"
            {
                "total_count": 17,
                "widgets": [
                    {
                        "name": "Steve",
                        "color": "aquamarine",
                        "power": 9001
                    },
                    {
                        "name": "Widget O'Malley",
                        "color": "taupe",
                        "power": 42
                    }
                ],
                "mode": "ponens"
            }
            "#};
            let page = serde_json::from_str::<Page<Widget>>(src).unwrap();
            assert_eq!(
                page,
                Page {
                    items: vec![
                        Widget {
                            name: "Steve".into(),
                            color: "aquamarine".into(),
                            power: 9001,
                        },
                        Widget {
                            name: "Widget O'Malley".into(),
                            color: "taupe".into(),
                            power: 42,
                        },
                    ],
                    total_count: Some(17),
                    incomplete_results: None,
                }
            );
        }

        #[test]
        fn from_map_extra_list_field() {
            let src = indoc! {r#"
            {
                "total_count": 17,
                "widgets": [
                    {
                        "name": "Steve",
                        "color": "aquamarine",
                        "power": 9001
                    },
                    {
                        "name": "Widget O'Malley",
                        "color": "taupe",
                        "power": 42
                    }
                ],
                "modes": ["ponens", "tollens"]
            }
            "#};
            let page = serde_json::from_str::<Page<Widget>>(src).unwrap();
            assert_eq!(
                page,
                Page {
                    items: vec![
                        Widget {
                            name: "Steve".into(),
                            color: "aquamarine".into(),
                            power: 9001,
                        },
                        Widget {
                            name: "Widget O'Malley".into(),
                            color: "taupe".into(),
                            power: 42,
                        },
                    ],
                    total_count: Some(17),
                    incomplete_results: None,
                }
            );
        }

        #[test]
        fn from_map_extra_item_list_field() {
            let src = indoc! {r#"
            {
                "total_count": 17,
                "widgets": [
                    {
                        "name": "Steve",
                        "color": "aquamarine",
                        "power": 9001
                    },
                    {
                        "name": "Widget O'Malley",
                        "color": "taupe",
                        "power": 42
                    }
                ],
                "more_widgets": [
                    {
                        "name": "Gidget",
                        "color": "chartreuse",
                        "power": 23
                    }
                ],
            }
            "#};
            assert!(serde_json::from_str::<Page<Widget>>(src).is_err());
        }

        #[test]
        fn from_map_extra_no_list_field() {
            let src = indoc! {r#"
            {
                "total_count": 0
            }
            "#};
            assert!(serde_json::from_str::<Page<Widget>>(src).is_err());
        }

        #[test]
        fn from_search_results() {
            let src = indoc! {r#"
            {
                "total_count": 100,
                "incomplete_results": true,
                "items": [
                    {
                        "name": "Steve",
                        "color": "aquamarine",
                        "power": 9001
                    },
                    {
                        "name": "Widget O'Malley",
                        "color": "taupe",
                        "power": 42
                    }
                ]
            }
            "#};
            let page = serde_json::from_str::<Page<Widget>>(src).unwrap();
            assert_eq!(
                page,
                Page {
                    items: vec![
                        Widget {
                            name: "Steve".into(),
                            color: "aquamarine".into(),
                            power: 9001,
                        },
                        Widget {
                            name: "Widget O'Malley".into(),
                            color: "taupe".into(),
                            power: 42,
                        },
                    ],
                    total_count: Some(100),
                    incomplete_results: Some(true),
                }
            );
        }
    }
}
