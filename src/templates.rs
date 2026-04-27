use minijinja::{Environment, Value};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "templates/"]
pub struct Templates;

pub struct TemplateEngine {
    env: Environment<'static>,
}

impl TemplateEngine {
    pub fn new() -> Result<Self, minijinja::Error> {
        let mut env = Environment::new();
        env.set_loader(|name: &str| {
            Ok(Templates::get(name)
                .map(|f| String::from_utf8_lossy(f.data.as_ref()).into_owned()))
        });
        env.add_filter("truncate",       filter_truncate);
        env.add_filter("filesizeformat", filter_filesize);
        env.add_filter("short_sha",      filter_short_sha);
        Ok(Self { env })
    }

    pub fn render(&self, name: &str, ctx: Value) -> Result<String, minijinja::Error> {
        self.env.get_template(name)?.render(ctx)
    }
}

fn filter_truncate(s: &str, len: Option<usize>) -> String {
    let n = len.unwrap_or(72);
    if s.chars().count() <= n { s.to_string() }
    else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

fn filter_filesize(bytes: Value) -> String {
    let b = bytes.as_i64().unwrap_or(0).max(0) as u64;
    match b {
        n if n < 1024           => format!("{n} B"),
        n if n < 1024 * 1024    => format!("{:.1} KB", n as f64 / 1024.0),
        n if n < 1024*1024*1024 => format!("{:.1} MB", n as f64 / 1_048_576.0),
        n                       => format!("{:.1} GB", n as f64 / 1_073_741_824.0),
    }
}

fn filter_short_sha(sha: &str) -> String {
    sha.chars().take(8).collect()
}
