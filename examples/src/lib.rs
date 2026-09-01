
pub struct Report {
    failed: Vec<String>,
}

impl Report {
    pub fn new() -> Self {
        Report { failed: Vec::new() }
    }

    pub fn check(&mut self, name: &str, r: Result<String, String>) {
        match r {
            Ok(info) => println!("PASS {name}: {info}"),
            Err(e) => {
                println!("FAIL {name}: {e}");
                self.failed.push(name.to_string());
            }
        }
    }

    // The closing line, and a nonzero exit when any case failed.
    pub fn finish(self, ok: &str) {
        if self.failed.is_empty() {
            println!("{ok}");
        } else {
            println!("FAILED: {}", self.failed.join(", "));
            std::process::exit(1);
        }
    }
}
