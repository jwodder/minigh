use anyhow::Context;
use clap::Parser;
use minigh::{Client, RequestError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::process::ExitCode;
use url::Url;

static QUERY: &str = indoc::indoc! {"
    query ($owner: String!, $cursor: String) {
        repositoryOwner (login: $owner) {
            repositories (
                first: 100,
                orderBy: {field: NAME, direction: ASC},
                ownerAffiliations: [OWNER],
                after: $cursor,
            ) {
                nodes {
                    nameWithOwner
                    description
                    repositoryTopics (first: 20) {
                        nodes {
                            topic {
                                name
                            }
                        }
                    }
                    primaryLanguage {
                        name
                    }
                    url
                    homepageUrl
                    stargazerCount
                    forkCount
                }
                pageInfo {
                    endCursor
                    hasNextPage
                }
            }
        }
    }
"};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct Response {
    data: ResponseData,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ResponseData {
    repository_owner: OwnerData,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct OwnerData {
    repositories: Repos,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Repos {
    nodes: Vec<GraphQLRepository>,
    page_info: PageInfo,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
struct GraphQLRepository {
    name_with_owner: String,
    description: Option<String>,
    repository_topics: Topics,
    url: Url,
    stargazer_count: u64,
    fork_count: u64,
    homepage_url: Option<String>,
    primary_language: Option<Named>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    end_cursor: Option<String>,
    has_next_page: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct Topics {
    nodes: Vec<TopicNode>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct TopicNode {
    topic: Named,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct Named {
    name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Repository {
    full_name: String,
    description: Option<String>,
    topics: Vec<String>,
    html_url: Url,
    stargazers_count: u64,
    forks_count: u64,
    homepage: Option<String>,
    language: Option<String>,
}

impl From<GraphQLRepository> for Repository {
    fn from(r: GraphQLRepository) -> Repository {
        Repository {
            full_name: r.name_with_owner,
            description: r.description,
            topics: r
                .repository_topics
                .nodes
                .into_iter()
                .map(|tn| tn.topic.name)
                .collect(),
            html_url: r.url,
            stargazers_count: r.stargazer_count,
            forks_count: r.fork_count,
            homepage: r.homepage_url,
            language: r.primary_language.map(|pl| pl.name),
        }
    }
}

#[derive(Clone, Debug, Eq, Parser, PartialEq)]
struct Arguments {
    #[arg(short = 'J', long)]
    json: bool,

    owner: String,
}

impl Arguments {
    fn run(&self) -> anyhow::Result<()> {
        let token = gh_token::get().context("Failed to fetch GitHub token")?;
        let client = Client::new(&token)?;
        let mut first = true;
        let mut cursor: Option<String> = None;
        loop {
            let mut variables =
                Map::from_iter([(String::from("owner"), Value::from(self.owner.clone()))]);
            if let Some(c) = cursor {
                variables.insert(String::from("cursor"), Value::from(c));
            }
            let resp: Response = client.graphql(QUERY, &variables)?;
            let page = resp.data.repository_owner.repositories;
            for repo in page.nodes {
                let repo = Repository::from(repo);
                if self.json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&repo)
                            .expect("serializing Repository should not fail")
                    );
                } else {
                    if !std::mem::replace(&mut first, false) {
                        println!();
                    }
                    println!("Repository: {}", repo.full_name);
                    println!("URL: {}", repo.html_url);
                    println!(
                        "Description: {}",
                        repo.description.as_deref().unwrap_or("-")
                    );
                    println!("Language: {}", repo.language.as_deref().unwrap_or("-"));
                    print!("Homepage: ");
                    if let Some(hp) = repo.homepage.as_ref().filter(|hp| !hp.is_empty()) {
                        println!("{hp}");
                    } else {
                        println!("-");
                    }
                    print!("Topics: ");
                    if repo.topics.is_empty() {
                        println!("-");
                    } else {
                        println!("{}", repo.topics.join(", "));
                    }
                    println!("Stars: {}", repo.stargazers_count);
                    println!("Forks: {}", repo.forks_count);
                }
            }
            cursor = page.page_info.end_cursor;
            if !page.page_info.has_next_page {
                break;
            }
        }
        Ok(())
    }
}

fn main() -> ExitCode {
    match Arguments::parse().run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e:?}");
            for src in e.chain() {
                if let Some(err) = src.downcast_ref::<RequestError>() {
                    if let Some(body) = err.body() {
                        eprintln!("\n{body}");
                    }
                    break;
                }
            }
            ExitCode::FAILURE
        }
    }
}
