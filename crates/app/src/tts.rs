//! macOS speech uses a child process, never a shell or a temporary text file.
use std::io::{self, Write};
use std::process::{Child, Command, Stdio};

#[derive(Default)]
pub struct Speech(Option<Child>);

impl Speech {
    pub fn start(&mut self, text: String, language: &str) -> io::Result<()> {
        self.stop();
        let voices = Command::new("/usr/bin/say").args(["-v", "?"]).output()?;
        let listing = String::from_utf8_lossy(&voices.stdout);
        let language = language.split(['-', '(']).next().unwrap_or(language);
        let voices: Vec<_> = listing
            .lines()
            .filter_map(|line| {
                let description = line.split('#').next()?.trim();
                let (name, locale) = description.rsplit_once(char::is_whitespace)?;
                (locale.split('_').next() == Some(language)).then_some(name.trim())
            })
            .collect();
        let voice = voices.iter().copied()
            .find(|name| matches!(*name, "Kyoko" | "Samantha"))
            .or_else(|| voices.first().copied())
            .ok_or_else(|| io::Error::other(format!("No installed speech voice for {language}. Add a voice in macOS System Settings → Accessibility → Spoken Content.")))?;
        let mut child = Command::new("/usr/bin/say")
            .args(["-v", voice, "-f", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let mut input = child.stdin.take().expect("piped stdin");
        // A long passage must not block the UI while say consumes its input.
        std::thread::spawn(move || {
            let _ = input.write_all(text.as_bytes());
        });
        self.0 = Some(child);
        Ok(())
    }

    pub fn finished(&mut self) -> io::Result<bool> {
        match self.0.as_mut().map(Child::try_wait).transpose()?.flatten() {
            Some(status) => {
                self.0 = None;
                if !status.success() {
                    return Err(io::Error::other("Speech synthesis failed"));
                }
                Ok(true)
            }
            None => Ok(self.0.is_none()),
        }
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Speech {
    fn drop(&mut self) {
        self.stop();
    }
}
