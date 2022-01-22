#![feature(let_else)]

use std::fs;
use std::path::{PathBuf, Path};
use std::time;

use eyre::{Result, WrapErr};

use structopt::StructOpt;

use tracing::{Level, info, debug};
use tracing_subscriber::prelude::*;

use twitchchat::{
    messages::{Commands, Privmsg},
    runner::AsyncRunner,
    connector,
    commands,
    Status,
    UserConfig,
};

#[derive(StructOpt)]
struct Opts {
    #[structopt(short, long, default_value = "autoplay.toml")]
    config: PathBuf,
}

#[derive(Debug, Clone, PartialEq, serde_derive::Deserialize)]
struct Config {
    user_config: UserConfig,
    channels: Vec<String>,
    wait_seconds: u64,
}

impl Config {
    fn load(p: &Path) -> Result<Self> {
        let contents = fs::read_to_string(p)
            .with_context(||
                format!("couldn't read config file {}", p.display()))?;
        let config = toml::from_str(&contents)
            .context("couldn't parse config file")?;
        Ok(config)
    }
}

struct App {
    config: Config,
    runner: AsyncRunner,
    next_allowed: time::Instant,
}

impl App {
    #[tracing::instrument(skip(config))]
    async fn connect(config: Config) -> Result<App> {
        let connector = connector::tokio::Connector::twitch()?;
        let mut runner =
            AsyncRunner::connect(connector, &config.user_config).await?;
        info!(identity = ?runner.identity, "connected");
        for channel in &config.channels {
            runner.join(&channel).await?;
            info!(?channel, "joined channel");
        }

        let next_allowed = time::Instant::now();

        Ok(App { config, runner, next_allowed })
    }

    #[tracing::instrument(skip(self))]
    async fn run(&mut self) -> Result<()> {
        loop {
            let status = self.runner.next_message().await?;
            debug!(message = ?status, "message");

            let Status::Message(message) = status else {
                break;
            };

            self.handle(&message).await?;
        }

        Ok(())
    }

    #[tracing::instrument(skip(self), level = "debug")]
    async fn handle(&mut self, message: &Commands<'_>) -> Result<()> {
        let Commands::Privmsg(privmsg) = message else {
            return Ok(());
        };

        if !self.is_interesting(&privmsg) {
            return Ok(());
        }

        if self.dont_spam() {
            return Ok(())
        }

        self.say_play(privmsg.channel()).await?;

        Ok(())
    }

    #[tracing::instrument(skip(self, privmsg), level = "debug")]
    fn is_interesting(&self, privmsg: &Privmsg) -> bool {
        if privmsg.name() == self.runner.identity.username() {
            debug!("ignoring message from self");
            return false;
        }

        if privmsg.name().ends_with("bot") {
            // implement half of https://ircbots.github.io/
            // "Automatic Replies Non-Proliferation Protocol"
            debug!("ignoring bot");
            return false;
        }

        let text = privmsg.data().trim().to_ascii_lowercase();
        if text != "!play" {
            // TODO: figure out which emotes also count
            // TODO: figure out if it really only counts when !play is
            // the entire message.
            debug!("ignoring non-!play message");
            return false;
        }

        return true;
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn dont_spam(&mut self) -> bool {
        let now = time::Instant::now();

        if now < self.next_allowed {
            debug!("not allowed yet");
            return true;
        }

        self.next_allowed =
            now + time::Duration::from_secs(self.config.wait_seconds);

        return false;
    }

    #[tracing::instrument(skip(self))]
    async fn say_play(&mut self, channel: &str) -> Result<()> {
        let mut w = self.runner.writer();
        w.encode(commands::privmsg(channel, "!play")).await?;
        Ok(())
    }
}

fn init_tracing() -> Result<()> {
    let filter = tracing_subscriber::filter::Targets::new()
        .with_target("autoplay", Level::DEBUG);
    use tracing_subscriber::fmt::format::FmtSpan;
    let subscriber = tracing_subscriber::fmt()
        .pretty()
        .with_thread_names(true)
        // enable everything
        .with_max_level(tracing::Level::DEBUG)
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        .finish()
        .with(filter);
    tracing::subscriber::set_global_default(subscriber)?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing().context("couldn't init tracing")?;

    let opts = Opts::from_args();
    let config = Config::load(&opts.config)?;

    let mut app = App::connect(config).await?;

    app.run().await?;

    Ok(())
}
