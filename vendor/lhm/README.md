# Vendored LibreHardwareMonitor runtime

These DLLs are the **minimal** set required to read the CPU package temperature
headless via `LibreHardwareMonitorLib` (verified by trimming until CPU sensing
stopped working). WatchDesk's sensor sidecar (`sidecar/WatchdeskSensors.cs`) is
compiled against `LibreHardwareMonitorLib.dll` and all of these are embedded into
`watchdesk.exe` at build time, then extracted next to the sidecar on `install`.

| File | Origin |
|------|--------|
| `LibreHardwareMonitorLib.dll` | LibreHardwareMonitor **0.9.6** |
| `System.Memory.dll` etc. | .NET Framework compatibility shims shipped with LibreHardwareMonitor 0.9.6 |

## License

`LibreHardwareMonitorLib.dll` is part of **LibreHardwareMonitor**, licensed under
the **Mozilla Public License 2.0 (MPL-2.0)**.

- Source: https://github.com/LibreHardwareMonitor/LibreHardwareMonitor
- Version: 0.9.6 (`0.9.6+3d331e3370efb858411f19511373eff65a218701`)

The DLLs are redistributed **unmodified**. MPL-2.0 permits redistribution of the
unmodified library as part of a larger work under other terms; WatchDesk itself
remains MIT-licensed. If you modify LibreHardwareMonitor, MPL-2.0 requires you to
make those file-level modifications available under MPL-2.0.

To update: rebuild/download LibreHardwareMonitor, replace `LibreHardwareMonitorLib.dll`
(and refresh the `System.*` shims if its target framework changed), and bump the
version above.
