#[path = "../utils/tokenizer.rs"]
mod tokenizer;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage:");
        eprintln!("  title-parse <title>             single torrent title");
        eprintln!("  title-parse --url <rss-url>     RSS preview (tokenizer chain)");
        std::process::exit(1);
    }
    if args[1] == "--url" {
        let url = args.get(2).expect("missing URL");
        preview_url(url);
    } else {
        parse_title(&args[1]);
    }
}

fn parse_title(raw: &str) {
    println!("RAW:       {raw}");
    println!("──────────────────────────");
    match tokenizer::parse_torrent_title(raw) {
        Some(p) => {
            println!("group:     {}", p.group.as_deref().unwrap_or("-"));
            println!("name:      {}", p.name.as_deref().unwrap_or("-"));
            println!("name_jp:   {}", p.name_jp.as_deref().unwrap_or("-"));
            println!(
                "season:    {}",
                p.season.map_or("-".into(), |s: u8| s.to_string())
            );
            println!(
                "episode:   {}",
                p.episode.map_or("-".into(), |e: f32| e.to_string())
            );
        }
        None => println!("❌ parse failed"),
    }
    println!(
        "  batch: {}",
        if tokenizer::is_batch_title(raw) {
            "yes"
        } else {
            "no"
        }
    );
}

fn preview_url(url: &str) {
    println!("URL:       {url}");
    println!("──────────────────────────");

    let body = match ureq::get(url)
        .call()
        .and_then(|r| r.into_string().map_err(|e| e.into()))
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("❌ HTTP: {e}");
            return;
        }
    };
    let ch = match body.parse::<rss::Channel>() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ XML: {e}");
            return;
        }
    };

    let channel_title = ch.title().to_string();
    let items: Vec<String> = ch
        .items()
        .iter()
        .take(5)
        .filter_map(|i| i.title().map(String::from))
        .collect();

    println!("channel:   {channel_title}");
    println!("items:     {}", items.len());
    println!();
    println!("── tokenizer on item titles ──");

    let mut name = String::new();
    let mut season: u8 = 1;
    for (i, t) in items.iter().enumerate() {
        if let Some(p) = tokenizer::parse_torrent_title(t) {
            let n = p.name.as_deref().unwrap_or("-");
            let s = p.season.unwrap_or(1);
            println!(
                "  item[{i}]: name=\"{n}\"  S{s}  ep={}  group={}",
                p.episode.map_or("-".into(), |e| e.to_string()),
                p.group.as_deref().unwrap_or("-")
            );
            if name.is_empty() && p.name.is_some() {
                name = p.name.unwrap();
                season = s;
            }
        } else {
            println!("  item[{i}]: parse failed");
        }
    }

    if name.is_empty() {
        name = items.first().cloned().unwrap_or_default();
    }

    println!();
    println!("RESULT:  \"{name}\"  S{season}");
    println!();
    for t in &items {
        println!("  {t}");
    }
}
