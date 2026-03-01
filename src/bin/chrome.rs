use std::process::Command;

fn chrome_active_tab() -> Option<(String, String)> {
    let url = Command::new("osascript")
        .arg("-e")
        .arg(r#"tell application "Google Chrome" to get URL of active tab of front window"#)
        .output()
        .ok()?;

    let title = Command::new("osascript")
        .arg("-e")
        .arg(r#"tell application "Google Chrome" to get title of active tab of front window"#)
        .output()
        .ok()?;

    if !url.status.success() || !title.status.success() {
        return None;
    }

    Some((
        String::from_utf8_lossy(&url.stdout).trim().to_string(),
        String::from_utf8_lossy(&title.stdout).trim().to_string(),
    ))
}

fn chrome_all_tabs() -> Option<Vec<(String, String)>> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(
            r#"
tell application "Google Chrome"
    set output to ""
    repeat with w in every window
        repeat with t in every tab of w
            set output to output & URL of t & "\t" & title of t & "\n"
        end repeat
    end repeat
    return output
end tell
"#,
        )
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let tabs: Vec<(String, String)> = text
        .lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let (url, title) = line.split_once('\t')?;
            Some((url.to_string(), title.to_string()))
        })
        .collect();

    Some(tabs)
}

fn main() {
    println!("=== Active Tab ===");
    match chrome_active_tab() {
        Some((url, title)) => {
            println!("Title: {title}");
            println!("URL:   {url}");
        }
        None => eprintln!("Failed to read Chrome active tab."),
    }

    println!("\n=== All Tabs ===");
    match chrome_all_tabs() {
        Some(tabs) => {
            for (i, (url, title)) in tabs.iter().enumerate() {
                println!("[{}] {}", i + 1, title);
                println!("    {url}");
            }
            println!("\nTotal: {} tabs", tabs.len());
        }
        None => eprintln!("Failed to read Chrome tabs. Is Chrome running?"),
    }
}
