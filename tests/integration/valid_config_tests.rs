//! Tests config files that have sometimes caused issues despite being valid.

use std::{io::Read, process::Stdio, thread, time::Duration};
#[cfg(feature = "default")]
use std::{io::Write, path::Path};

use predicates::prelude::*;

use crate::util::{mon_command, spawn_mon_in_pty};

fn reader_to_string(mut reader: Box<dyn Read>) -> String {
    let mut buf = String::default();
    reader.read_to_string(&mut buf).unwrap();

    buf
}

fn run_and_kill(args: &[&str]) {
    let (master, mut handle) = spawn_mon_in_pty(args);
    let reader = master.try_clone_reader().unwrap();
    let _ = master.take_writer().unwrap();

    const TIMES_CHECKED: u64 = 6; // Check 6 times, once every 500ms, for 3 seconds total.

    for _ in 0..TIMES_CHECKED {
        thread::sleep(Duration::from_millis(500));
        match handle.try_wait() {
            Ok(Some(exit)) => {
                println!("output: {}", reader_to_string(reader));
                panic!("program terminated unexpectedly (exit status: {exit:?})");
            }
            Err(e) => {
                println!("output: {}", reader_to_string(reader));
                panic!("error while trying to wait: {e}")
            }
            _ => {}
        }
    }

    handle.kill().unwrap();
}

fn run_and_kill_cfg(path: &str) {
    run_and_kill(&["-C", path]);
}

/// Run for a moment, then return whatever was drawn to the pty.
///
/// Unlike [`run_and_kill`], this asserts on the *contents* of a frame rather than just on
/// the process staying alive, which is what it takes to catch a widget that parses fine but
/// never gets dispatched to.
fn run_and_capture(args: &[&str]) -> String {
    capture(args, None).0
}

/// Run `mon`, optionally press a key partway through, and return what it drew.
///
/// The second element is only what arrived *after* the keypress, which is what a test about
/// a key has to look at: ratatui re-emits changed cells, so a key that moves a highlight
/// shows up as those cells and nothing else. Asserting on the whole frame would pass
/// whether or not the key did anything.
fn capture(args: &[&str], key: Option<&str>) -> (String, String) {
    use std::io::Write as _;

    let (master, mut handle) = spawn_mon_in_pty(args);
    let reader = master.try_clone_reader().unwrap();

    // Held, not dropped. `mon` emits a device-status query (`ESC [ 6 n`) during startup and
    // waits for the terminal to answer; closing the write side means nothing ever can, and
    // the app sits there having drawn only its init sequence. That is what a bound `_`
    // binding was doing here -- `let _ = ...` drops at the end of the statement.
    let mut writer = master.take_writer().unwrap();

    // Drained on a thread. A single `read` returns whatever one chunk happens to hold,
    // which at startup is the terminal setup and nothing else; the frame arrives later.
    let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&collected);

    thread::spawn(move || {
        let mut reader = reader;
        let mut chunk = vec![0u8; 64 * 1024];

        while let Ok(read) = reader.read(&mut chunk) {
            if read == 0 {
                break;
            }

            match sink.lock() {
                Ok(mut sink) => sink.extend_from_slice(&chunk[..read]),
                Err(_) => break,
            }
        }
    });

    let snapshot = || match collected.lock() {
        Ok(buf) => buf.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };

    // Long enough for a first collection tick and a draw.
    thread::sleep(Duration::from_millis(2500));

    if let Ok(Some(exit)) = handle.try_wait() {
        panic!("program terminated unexpectedly (exit status: {exit:?})");
    }

    let before = snapshot().len();

    if let Some(key) = key {
        writer.write_all(key.as_bytes()).unwrap();
        writer.flush().unwrap();

        // A range key travels to the collection thread, waits for the scan to roll the
        // history up onto the new span, and only then comes back as a redraw.
        thread::sleep(Duration::from_millis(2500));
    }

    let buf = snapshot();
    handle.kill().unwrap();

    (
        String::from_utf8_lossy(&buf).into_owned(),
        String::from_utf8_lossy(&buf[before..]).into_owned(),
    )
}

/// Strip terminal escape sequences, leaving the characters that were drawn.
fn drawn_text(raw: &str) -> String {
    let escapes = regex::Regex::new(r"\x1b\[[0-9;?]*[a-zA-Z]").unwrap();
    escapes.replace_all(raw, "").into_owned()
}

#[test]
fn test_basic() {
    run_and_kill(&[]);
}

/// A test to ensure that a bad config will fail the `run_and_kill` function.
#[test]
#[should_panic]
fn test_bad_basic() {
    run_and_kill(&["--this_does_not_exist"]);
}

#[test]
fn test_empty() {
    run_and_kill_cfg("./tests/valid_configs/empty_config.toml");
}

#[cfg(feature = "default")]
fn test_uncommented_default_config(original: &Path, test_name: &str) {
    use regex::Regex;

    // Take the default config file and uncomment everything.
    let default_config = match std::fs::File::open(original) {
        Ok(mut default_config_file) => {
            let mut buf = String::new();
            default_config_file
                .read_to_string(&mut buf)
                .expect("can read file");

            buf
        }
        Err(err) => {
            println!("Could not open default config, skipping {test_name}. Error: {err:?}");
            return;
        }
    };

    let default_config = Regex::new(r"(?m)^#([a-zA-Z\[])")
        .unwrap()
        .replace_all(&default_config, "$1");

    let default_config = Regex::new(r"(?m)^#(\s\s+)([a-zA-Z\[])")
        .unwrap()
        .replace_all(&default_config, "$2");

    let mut uncommented_config = match tempfile::NamedTempFile::new() {
        Ok(tf) => tf,
        Err(err) => {
            println!("Could not create a temp file, skipping {test_name}. Error: {err:?}");
            return;
        }
    };

    if let Err(err) = uncommented_config.write_all(default_config.as_bytes()) {
        println!("Could not write to temp file, skipping {test_name}. Error: {err:?}");
        return;
    }

    run_and_kill(&["-C", &uncommented_config.path().to_string_lossy()]);

    uncommented_config.close().unwrap();
}

#[cfg(feature = "default")]
#[test]
fn test_default() {
    test_uncommented_default_config(
        Path::new("./sample_configs/default_config.toml"),
        "test_default",
    );
}

#[cfg(feature = "default")]
#[test]
fn test_new_default() {
    use tempfile::TempPath;

    let new_temp_default_path = match tempfile::NamedTempFile::new() {
        Ok(temp_file) => temp_file.into_temp_path(),
        Err(err) => {
            println!("Could not create a temp file, skipping test_new_default. Error: {err:?}");
            return;
        }
    };

    // This is a hack because we need a temp file that doesn't exist.
    let actual_temp_default_path = new_temp_default_path.to_path_buf();
    new_temp_default_path.close().unwrap();

    if !actual_temp_default_path.exists() {
        run_and_kill(&["-C", &(actual_temp_default_path.to_string_lossy())]);

        // Re-take control over the temp path to ensure it gets deleted.
        let actual_temp_default_path =
            TempPath::try_from_path(actual_temp_default_path).expect("temp path exists");
        test_uncommented_default_config(&actual_temp_default_path, "test_new_default");

        actual_temp_default_path.close().unwrap();
    } else {
        println!("temp path we want to check exists, skip test_new_default test.");
    }
}

#[test]
fn test_demo() {
    let path: &str = "./sample_configs/demo_config.toml";
    if std::path::Path::new(path).exists() {
        run_and_kill(&["-C", path]);
    } else {
        println!("Could not read demo config.");
    }
}

/// The Claude-only sample layout has to keep parsing and drawing, since it is the config
/// used for hand-testing those widgets.
#[test]
fn test_claude() {
    let path: &str = "./sample_configs/claude_config.toml";
    if std::path::Path::new(path).exists() {
        run_and_kill(&["-C", path]);
    } else {
        println!("Could not read claude config.");
    }
}

#[test]
fn test_many_proc() {
    run_and_kill_cfg("./tests/valid_configs/many_proc.toml");
}

#[test]
fn test_all_proc() {
    run_and_kill_cfg("./tests/valid_configs/all_proc.toml");
}

#[test]
fn test_cpu_doughnut() {
    run_and_kill_cfg("./tests/valid_configs/cpu_doughnut.toml");
}

#[test]
fn test_theme() {
    run_and_kill_cfg("./tests/valid_configs/theme.toml");
}

#[test]
fn test_styling_sanity_check() {
    run_and_kill_cfg("./tests/valid_configs/styling.toml");
}

#[test]
fn test_styling_sanity_check_2() {
    run_and_kill_cfg("./tests/valid_configs/all_styling.toml");
}

#[test]
fn test_color_spelling_is_valid() {
    run_and_kill_cfg("./tests/valid_configs/all_styling_color.toml");
}

#[test]
fn test_filtering() {
    run_and_kill_cfg("./tests/valid_configs/filtering.toml");
}

#[test]
fn test_proc_columns() {
    run_and_kill_cfg("./tests/valid_configs/proc_columns.toml");
}

#[cfg(target_os = "linux")]
#[test]
fn test_linux_only() {
    run_and_kill_cfg("./tests/valid_configs/os_specific/linux.toml");
}

#[test]
fn test_temp_disk_sort_columns() {
    run_and_kill_cfg("./tests/valid_configs/temp_disk_sort_columns.toml");
}

#[test]
fn test_proc_default_sort() {
    run_and_kill_cfg("./tests/valid_configs/proc_default_sort.toml");
}

#[test]
fn test_newer_memory() {
    run_and_kill_cfg("./tests/valid_configs/widget/memory.toml");
}

#[test]
fn test_newer_cpu() {
    run_and_kill_cfg("./tests/valid_configs/widget/cpu.toml");
}

#[test]
fn test_newer_processes() {
    run_and_kill_cfg("./tests/valid_configs/widget/processes.toml");
}

#[test]
fn test_newer_network() {
    run_and_kill_cfg("./tests/valid_configs/widget/network.toml");
}

#[test]
fn test_network_alias() {
    run_and_kill_cfg("./tests/valid_configs/network_alias.toml");
}

/// This uses deprecated network settings - once they are removed, this test file should be moved to invalid configs.
#[test]
fn test_deprecated_network() {
    run_and_kill_cfg("./tests/valid_configs/deprecated/network.toml");
}

/// This uses deprecated process settings - once they are removed, this test file should be moved to invalid configs.
#[test]
fn test_deprecated_processes() {
    run_and_kill_cfg("./tests/valid_configs/deprecated/processes.toml");
}

/// This uses deprecated CPU settings - once they are removed, this test file should be moved to invalid configs.
#[test]
fn test_deprecated_cpu() {
    run_and_kill_cfg("./tests/valid_configs/deprecated/cpu.toml");
}

/// This uses deprecated memory settings - once they are removed, this test file should be moved to invalid configs.
#[test]
fn test_deprecated_memory() {
    run_and_kill_cfg("./tests/valid_configs/deprecated/memory.toml");
}

#[test]
fn test_newer_temperature() {
    run_and_kill_cfg("./tests/valid_configs/widget/temperature.toml");
}

#[test]
fn test_disk_io_graph() {
    run_and_kill_cfg("./tests/valid_configs/widget/disk_io_graph.toml");
}

/// The power widget has to actually *draw*, not just parse and stay alive.
///
/// The three dispatch sites in `canvas.rs` all end in `_ => {}`, so a missing arm compiles
/// clean and silently renders nothing. Asserting the rendered title reaches the pty is the
/// only thing that catches it.
/// Both Claude widgets have to actually draw, for the same reason the power widget does.
#[test]
fn test_claude_widgets_render() {
    let rendered = run_and_capture(&["-C", "./tests/valid_configs/widget/claude.toml"]);

    assert!(
        !rendered.trim().is_empty(),
        "the claude layout rendered an empty buffer"
    );
    assert!(
        rendered.contains("Claude Sessions"),
        "the sessions table did not draw its title -- likely a missing dispatch arm in \
         `canvas.rs`. Rendered output was:\n{rendered}"
    );
    assert!(
        rendered.contains("Claude Stats"),
        "the stats graph did not draw its title. Rendered output was:\n{rendered}"
    );

    // The footer is painted by the widget into rows the chart reserved through its block
    // padding. If that reservation ever stops working the graph draws over these instead,
    // and nothing else in the test suite would notice.
    assert!(
        rendered.contains("30m"),
        "the range selector row is missing. Rendered output was:\n{rendered}"
    );
    // Either the legend or the scan note, because they share the row: on a machine with a
    // real transcript tree the first couple of seconds go on the cold read, and saying so
    // is what that row is for.
    assert!(
        rendered.contains('\u{25cf}') || rendered.contains("scanning transcripts"),
        "the legend row drew neither swatches nor a scan note. Rendered output was:\n{rendered}"
    );

    // Absolute clock times on the x-axis, rather than the `{secs}s` fallback every other
    // graph uses. A machine that has never run Claude Code still gets an axis.
    let clock = regex::Regex::new(r"[0-2][0-9]:[0-5][0-9]").unwrap();
    assert!(
        clock.is_match(&rendered),
        "no clock-time x-axis label. Rendered output was:\n{rendered}"
    );
}

/// Pressing a range key has to reach the collection thread and come back as a redraw.
///
/// This is the one test that ties the whole path together: the key handler's focus check,
/// the modifier gate, `CollectionThreadEvent::SetClaudeStatsRange`, the scan thread rolling
/// the history up onto the new span, and the footer drawing the new highlight. Every piece
/// of it is unit-tested in isolation and none of that would notice the wiring being wrong.
#[test]
fn a_range_key_moves_the_stats_graph_to_another_span() {
    // The sample layout starts on `2h` and focuses the graph, so `+` shortens it to `30m`.
    let (_, after) = capture(&["-C", "./sample_configs/claude_config.toml"], Some("+"));
    let redrawn = drawn_text(&after);

    assert!(
        redrawn.contains("30m"),
        "`30m` never took the highlight. Redraw after the keypress was:\n{redrawn}"
    );
    assert!(
        redrawn.contains("2h"),
        "`2h` never gave up the highlight. Redraw after the keypress was:\n{redrawn}"
    );

    // The axis span moved with it, which is the half that proves the *data* followed the
    // key rather than only the selector row repainting.
    let clock = regex::Regex::new(r"[0-2][0-9]:[0-5][0-9]").unwrap();
    assert!(
        clock.is_match(&redrawn),
        "the x-axis did not relabel. Redraw after the keypress was:\n{redrawn}"
    );
}

#[test]
fn test_power_graph_renders() {
    let rendered = run_and_capture(&["-C", "./tests/valid_configs/widget/power.toml"]);

    assert!(
        !rendered.trim().is_empty(),
        "the power layout rendered an empty buffer"
    );
    assert!(
        rendered.contains("Power"),
        "the power widget did not draw its title -- likely a missing dispatch arm in \
         `canvas.rs`. Rendered output was:\n{rendered}"
    );
}

/// This uses deprecated temperature settings - once they are removed, this test file should be moved to invalid configs.
#[test]
fn test_deprecated_temperature() {
    run_and_kill_cfg("./tests/valid_configs/deprecated/temperature.toml");
}

/// Test that deprecated warnings are not shown for config options that are not actually set,
/// even when a `[flags]` section is present.
#[test]
fn test_no_spurious_deprecated_warnings() {
    let mut child = mon_command(&["-C", "./tests/valid_configs/empty_flags.toml"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    thread::sleep(Duration::from_secs(1));
    child.kill().unwrap();
    child.wait().unwrap();

    let stderr_str = {
        let mut stderr = child.stderr.take().unwrap();
        let mut buf = String::new();

        stderr.read_to_string(&mut buf).unwrap();
        buf
    };

    assert!(
        predicate::str::contains("deprecated")
            .not()
            .eval(&stderr_str),
        "Expected no deprecated warnings, but got: {stderr_str}"
    );
}
