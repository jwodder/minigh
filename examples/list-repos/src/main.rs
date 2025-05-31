use clap::Parser;
use minigh::{Client, StatusError};
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

fn main() -> ExitCode {
    let args = Arguments::parse();
    let token = match gh_token::get() {
        Ok(token) => token,
        Err(e) => {
            eprintln!("Failed to fetch GitHub token: {e}");
            return ExitCode::FAILURE;
        }
    };
    let client = Client::new(&token);
    let mut first = true;
    match client.paginate::<Repository>(&format!("/users/{}/repos", args.owner)) {
        Ok(repos) => {
            for repo in repos {
                if args.json {
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
            ExitCode::SUCCESS
        }
        Err(e) => {
            // Use anyhow to display the error chain
            let e = anyhow::Error::new(e);
            eprintln!("{e:?}");
            if let Some(body) = e
                .downcast_ref::<StatusError>()
                .and_then(StatusError::body)
                .filter(|s| !s.is_empty())
            {
                eprintln!("\n{body}");
            }
            ExitCode::FAILURE
        }
    }
}
