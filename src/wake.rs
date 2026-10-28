//! Task Scheduler definition for the optional "start Focus Island at the next reminder" feature.
//!
//! This module only builds the XML (pure, testable on any OS). Registering it with
//! `schtasks.exe` is done by the Windows platform module, and only after the user turns the
//! setting on. The XML is used (rather than `/SC ONCE /SD <date>`) because it is
//! locale-independent: `/SD` expects the regional short-date format.

use chrono::NaiveDateTime;

/// Name of the scheduled task, in a dedicated folder so it is easy to find and remove.
pub const WAKE_TASK_NAME: &str = "FocusIsland\\Reminder Wake";
/// Argument the scheduled task passes. A second instance exits immediately if one is running.
pub const WAKE_ARG: &str = "--wake";

fn xml_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c => o.push(c),
        }
    }
    o
}

/// Task Scheduler 1.2 XML: one time trigger at `start_local`, runs `exe --wake` as the current
/// user with limited rights, only while that user is logged on. `StartWhenAvailable` runs it
/// late if the PC was off at that time; the app's missed-reminder policy then applies.
pub fn task_xml(exe_path: &str, start_local: NaiveDateTime) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Starts Focus Island so it can show a reminder. Created by Focus Island; remove it in Focus Island settings.</Description>
  </RegistrationInfo>
  <Triggers>
    <TimeTrigger>
      <StartBoundary>{start}</StartBoundary>
      <Enabled>true</Enabled>
    </TimeTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <Arguments>{arg}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        start = start_local.format("%Y-%m-%dT%H:%M:%S"),
        exe = xml_escape(exe_path),
        arg = WAKE_ARG,
    )
}

/// `schtasks /XML` expects UTF-16 (the declaration above says so): little-endian with a BOM.
pub fn encode_utf16le_bom(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + s.len() * 2);
    out.extend_from_slice(&[0xFF, 0xFE]);
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn xml_has_trigger_command_and_escaping() {
        let start = NaiveDate::from_ymd_opt(2026, 10, 25)
            .unwrap()
            .and_hms_opt(9, 5, 0)
            .unwrap();
        let xml = task_xml(r#"C:\Apps & Co\Focus <Island>\focus-island.exe"#, start);
        assert!(xml.contains("<StartBoundary>2026-10-25T09:05:00</StartBoundary>"));
        assert!(xml.contains(
            r#"<Command>C:\Apps &amp; Co\Focus &lt;Island&gt;\focus-island.exe</Command>"#
        ));
        assert!(xml.contains("<Arguments>--wake</Arguments>"));
        assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(xml.contains("<WakeToRun>false</WakeToRun>"));
    }

    #[test]
    fn utf16_encoding_has_bom_and_le_units() {
        let b = encode_utf16le_bom("A\u{e9}");
        assert_eq!(b, vec![0xFF, 0xFE, 0x41, 0x00, 0xE9, 0x00]);
    }
}
