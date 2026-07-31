use bangumi_rss::tokenizer;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage:");
        eprintln!("  title-parse <title>             parse a single torrent title");
        eprintln!("  title-parse --url <rss-url>     preview all items in an RSS feed");
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
    println!("┌──────────────────────────────────┐");
    println!("│  title: {raw}");
    println!("└──────────────────────────────────┘");
    match tokenizer::parse_torrent_title(raw) {
        Some(p) => {
            println!("  name:     {}", p.name.as_deref().unwrap_or("-"));
            println!("  name_jp:  {}", p.name_jp.as_deref().unwrap_or("-"));
            println!("  group:    {}", p.group.as_deref().unwrap_or("-"));
            println!(
                "  season:   {}",
                p.season.map_or("-".into(), |s: u8| format!("S{s}"))
            );
            println!(
                "  episode:  {}",
                p.episode.map_or("-".into(), |e: f32| e.to_string())
            );
        }
        None => println!("  ❌ parse failed"),
    }
}

fn preview_url(url: &str) {
    let resp = match ureq::get(url).call() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("❌ HTTP: {e}");
            return;
        }
    };
    let body = match resp.into_string() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("❌ HTTP read: {e}");
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

    println!("channel: {}", ch.title());
    println!("items:   {}", ch.items().len());
    println!();

    let mut first_name = String::new();
    for (i, item) in ch.items().iter().enumerate() {
        let title = item.title().unwrap_or("-");
        let torrent_url = item
            .enclosure()
            .map(|e| e.url())
            .or_else(|| item.link())
            .unwrap_or("");

        println!("── [{i}] ──────────────────────");
        println!("  raw:  {title}");
        if !torrent_url.is_empty() {
            println!("  url:  {torrent_url}");
        }

        match tokenizer::parse_torrent_title(title) {
            Some(p) => {
                let mut parts = Vec::new();
                if let Some(n) = &p.name {
                    parts.push(format!("name=\"{n}\""));
                    first_name = n.clone();
                }
                if let Some(s) = p.season {
                    parts.push(format!("S{s}"));
                }
                if let Some(e) = p.episode {
                    parts.push(format!("ep={e}"));
                }
                if let Some(g) = &p.group {
                    parts.push(format!("group=\"{g}\""));
                }
                println!("  → {}", parts.join("  "));
            }
            None => println!("  → ❌"),
        }
    }

    if !first_name.is_empty() {
        println!();
        println!("suggested_name: \"{first_name}\"");
    }
}
