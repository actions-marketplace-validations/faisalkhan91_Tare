//! `tare-mcp` entrypoint: a stdio JSON-RPC MCP server over a Tare store.
//! Usage: `tare-mcp [--db PATH] [--pricing FILE]`. Reads requests on stdin, writes responses
//! on stdout (line-delimited). Read-only; all figures are estimated.

use tare_core::PricingTable;
use tare_store::Store;

const SHIPPED_PRICING: &str = include_str!("../../pricing/pricing.json");
const USAGE: &str = "Usage: tare-mcp [--db PATH] [--pricing FILE]";

#[derive(Debug, PartialEq, Eq)]
enum ParsedArgs {
    Help,
    Serve { db: String, pricing: Option<String> },
}

fn parse_args(args: &[String], env_db: Option<String>) -> Result<ParsedArgs, String> {
    let mut db = None;
    let mut pricing = None;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if matches!(arg.as_str(), "-h" | "--help") {
            if args.len() != 1 {
                return Err("--help cannot be combined with other arguments".into());
            }
            return Ok(ParsedArgs::Help);
        }
        let (name, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(name, value)| (name, Some(value)));
        if !matches!(name, "--db" | "--pricing") {
            return Err(format!("unknown argument {arg:?}\n{USAGE}"));
        }
        let value = match inline {
            Some("") => return Err(format!("{name} requires a non-empty value")),
            Some(value) => value,
            None => {
                index += 1;
                match args.get(index) {
                    Some(value) if !value.starts_with('-') && !value.is_empty() => value,
                    _ => return Err(format!("{name} requires a value")),
                }
            }
        };
        let slot = if name == "--db" {
            &mut db
        } else {
            &mut pricing
        };
        if slot.replace(value.to_string()).is_some() {
            return Err(format!("{name} may only be supplied once"));
        }
        index += 1;
    }

    let db = db.or(env_db).unwrap_or_else(|| "tare.db".to_string());
    if db.trim().is_empty() {
        return Err("database path must not be empty".into());
    }
    Ok(ParsedArgs::Serve { db, pricing })
}

fn optional_env(name: &str) -> Result<Option<String>, String> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{name} is not valid Unicode")),
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (db, pricing_path) = match parse_args(&args, optional_env("TARE_DB")?)? {
        ParsedArgs::Help => {
            println!("{USAGE}");
            return Ok(());
        }
        ParsedArgs::Serve { db, pricing } => (db, pricing),
    };
    let pricing = match pricing_path.as_deref() {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|s| {
                if std::path::Path::new(path)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"))
                {
                    PricingTable::from_toml_str(&s)
                } else {
                    PricingTable::from_json_str(&s)
                }
            }),
        None => PricingTable::from_json_str(SHIPPED_PRICING),
    }?;
    let store = Store::open(&db)?;
    tare_mcp::serve_stdio(&store, &pricing)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("tare-mcp: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_args, ParsedArgs};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn parser_supports_both_value_forms_and_env_fallback() {
        assert_eq!(
            parse_args(&args(&["--db=data.db", "--pricing", "rates.toml"]), None).unwrap(),
            ParsedArgs::Serve {
                db: "data.db".into(),
                pricing: Some("rates.toml".into()),
            }
        );
        assert_eq!(
            parse_args(&[], Some("from-env.db".into())).unwrap(),
            ParsedArgs::Serve {
                db: "from-env.db".into(),
                pricing: None,
            }
        );
    }

    #[test]
    fn parser_rejects_missing_duplicate_and_unknown_values() {
        assert!(parse_args(&args(&["--db"]), None).is_err());
        assert!(parse_args(&args(&["--db", "--pricing", "p.json"]), None).is_err());
        assert!(parse_args(&args(&["--db=a", "--db=b"]), None).is_err());
        assert!(parse_args(&args(&["--mystery"]), None).is_err());
        assert!(parse_args(&args(&["position"]), None).is_err());
        assert!(parse_args(&args(&["--pricing="]), None).is_err());
    }
}
