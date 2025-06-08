use anyhow::Context;
use clap::Parser;
use minigh::{Client, RequestError};
use serde::{Deserialize, Serialize};
use std::process::ExitCode;
use url::Url;

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
        for r in client.paginate::<Repository>(&format!("/users/{}/repos", self.owner)) {
            let repo = r?;
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
