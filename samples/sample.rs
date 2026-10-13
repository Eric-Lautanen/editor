// Readitor sample code: exercises the lightweight tint.
use std::collections::HashMap;

const MAX_ROWS: usize = 2000;

pub struct Sheet {
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

pub fn parse(text: &str) -> Sheet {
    let mut header = Vec::new();
    let mut rows = Vec::new();
    let mut first = true;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue; // skip blanks
        }
        let fields: Vec<String> = line.split(',').map(|s| s.trim().to_string()).collect();
        if first {
            header = fields;
            first = false;
        } else {
            rows.push(fields);
        }
    }
    Sheet { header, rows }
}

fn main() {
    let text = "a,b\n1,2\n";
    let sheet = parse(text);
    println!("{} cols, {} rows", sheet.header.len(), sheet.rows.len());
}
