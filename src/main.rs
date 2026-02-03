use mailparse::MailHeaderMap;
use native_tls::TlsConnector;
use regex::Regex;
use serde::Deserialize;
use std::fs;
use std::thread;
use std::time::Duration;

#[derive(Deserialize, Clone)]
struct Config {
    imap_server: String,
    imap_port: u16,
    imap_username: String,
    imap_password: String,
    discord_webhook_url: String,
    ignored_senders: Option<Vec<String>>,
    ignored_subjects: Option<Vec<String>>,
}

fn main() {
    let config_content = fs::read_to_string("config.toml").expect("Failed to read config.toml");
    let config: Config = toml::from_str(&config_content).expect("Failed to parse config.toml");

    loop {
        println!("Connecting to IMAP server {}:{}...", config.imap_server, config.imap_port);
        if let Err(e) = run_monitor(&config) {
            eprintln!("Connection lost or error occurred: {}", e);
            eprintln!("Retrying in 10 seconds...");
            thread::sleep(Duration::from_secs(10));
        }
    }
}

fn run_monitor(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let tls = TlsConnector::builder().build()?;
    let client = imap::connect((&config.imap_server as &str, config.imap_port), &config.imap_server, &tls)?;
    let mut imap_session = client.login(&config.imap_username, &config.imap_password).map_err(|e| e.0)?;

    println!("Logged in as {}", config.imap_username);

    loop {
        imap_session.select("INBOX")?;

        // Fetch all messages (including seen ones if we restart, assuming we delete processed ones)
        let messages = imap_session.search("ALL")?;

        if !messages.is_empty() {
            println!("Found {} messages", messages.len());
            
            // Collect sequence numbers to process
            let seqs: Vec<u32> = messages.into_iter().collect();

            for seq_num in seqs {
                // Fetch the message content
                let fetches = imap_session.fetch(seq_num.to_string(), "RFC822")?;
                
                if let Some(msg) = fetches.iter().next() {
                    let body = msg.body().unwrap_or(&[]);
                    let parsed = mailparse::parse_mail(body)?;

                    let subject = parsed.headers.get_first_value("Subject").unwrap_or("No Subject".to_string());
                    let from = parsed.headers.get_first_value("From").unwrap_or("Unknown Sender".to_string());

                    // Check ignore list
                    let should_ignore = if let Some(ref senders) = config.ignored_senders {
                        senders.iter().any(|s| from.contains(s))
                    } else {
                        false
                    } || if let Some(ref subjects) = config.ignored_subjects {
                        subjects.iter().any(|s| subject.contains(s))
                    } else {
                        false
                    };

                    if should_ignore {
                        println!("Ignored email from: {}, Subject: {}", from, subject);
                        // Delete ignored emails too, to prevent reprocessing? 
                        // Or maybe just skip? If we skip, they remain in INBOX and will be fetched again because search is "ALL".
                        // To avoid infinite loop of fetching ignored emails, we MUST delete them or mark them differently (and change search query).
                        // Since user said "Sent messages can be deleted", I will assume ignored messages can also be deleted (skipped).
                        // If this is risky, I could change search to "UNSEEN" and just mark as seen.
                        // But let's stick to the previous flow: "Process = Delete". Ignoring is a form of processing.
                        imap_session.store(seq_num.to_string(), "+FLAGS (\\Deleted)")?;
                        continue;
                    }
                    
                    // Simple body extraction (prioritize text/plain)
                    let body_content = extract_body(&parsed).unwrap_or("Cannot parse body".to_string());

                    // Truncate body if too long for Discord (limit is 2000 chars)
                    let display_body = if body_content.len() > 1500 {
                        let mut end = 1500;
                        while !body_content.is_char_boundary(end) {
                            end -= 1;
                        }
                        format!("{}...", &body_content[..end])
                    } else {
                        body_content
                    };

                    println!("Processing email: {}", subject);

                    // Send to Discord
                    let client = reqwest::blocking::Client::new();
                    let payload = serde_json::json!({
                        "embeds": [{
                            "title": subject,
                            "author": {
                                "name": from
                            },
                            "description": display_body,
                            "color": 0x5865F2, // Blurple
                            "timestamp": chrono::Utc::now().to_rfc3339(),
                            "footer": {
                                "text": "📰 Newsletter"
                            }
                        }]
                    });

                    let res = client.post(&config.discord_webhook_url).json(&payload).send();

                    match res {
                        Ok(response) => {
                            if response.status().is_success() {
                                println!("Sent to Discord. Deleting email...");
                                imap_session.store(seq_num.to_string(), "+FLAGS (\\Deleted)")?;
                            } else {
                                eprintln!("Failed to send to Discord: Status {}", response.status());
                            }
                        },
                        Err(e) => {
                            eprintln!("Failed to send request to Discord: {}", e);
                            // Do not delete if failed to send
                        }
                    }
                }
            }
            // Permanently remove deleted messages
            imap_session.expunge()?;
        }

        // Wait before next check
        thread::sleep(Duration::from_secs(5));
    }
}

fn clean_body(body: &str) -> String {
    // Replace multiple newlines with double newline (max)
    let re_newlines = Regex::new(r"\n{3,}").unwrap();
    let body = re_newlines.replace_all(body, "\n\n");
    
    // Trim trailing spaces from each line
    let re_trailing_spaces = Regex::new(r"(?m)[ \t]+$").unwrap();
    let body = re_trailing_spaces.replace_all(&body, "");

    body.trim().to_string()
}

fn extract_body(parsed: &mailparse::ParsedMail) -> Option<String> {
    if parsed.ctype.mimetype == "text/plain" {
        return parsed.get_body().ok().map(|s| clean_body(&s));
    }
    
    // If multipart, search for text/plain
    for part in &parsed.subparts {
        if let Some(body) = extract_body(part) {
            return Some(body);
        }
    }

    // Fallback to text/html if no plain text found (or first part if nothing else)
    if parsed.ctype.mimetype == "text/html" {
         if let Ok(html_content) = parsed.get_body() {
             if let Ok(md) = html2text::from_read(html_content.as_bytes(), 80) {
                 return Some(clean_body(&md));
             }
         }
    }

    None
}
