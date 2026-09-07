// SPDX-License-Identifier: AGPL-3.0-only
//! Explicitly invoked synthetic fixture creator, not part of the Hub product.
#[path = "../tests/interop/seed.rs"]
mod seed;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 2 && args[0] == "--advance" {
        return seed::advance(std::path::Path::new(&args[1]));
    }
    if args.len() != 4 || args[0] != "--output" || args[2] != "--port" {
        return Err("usage: interop_fixture --output NEW_ABSOLUTE_DIRECTORY --port PORT".into());
    }
    let prepared = seed::prepare(std::path::Path::new(&args[1]), args[3].parse()?)?;
    // Only paths/public synthetic identities are printed; invitation stays in
    // an owner-only file and is never sent to the terminal.
    println!("{}", serde_json::to_string(&prepared)?);
    Ok(())
}
