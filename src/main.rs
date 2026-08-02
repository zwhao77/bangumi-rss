use envconfig::Envconfig;

use bangumi_rss::config::Config;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug"))
        .format_timestamp_millis()
        .init();

    let config = Config::init_from_env()?;
    bangumi_rss::run(config)
}
