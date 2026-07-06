// WatchDesk sensor sidecar.
//
// Hosts LibreHardwareMonitorLib headless (no GUI) and streams the CPU package
// temperature as one JSON line per interval on stdout, e.g. {"cpu_temp_c":50.4}.
// The WatchDesk service (Rust) spawns this and reads its output.
//
// The service runs as LocalSystem (elevated), which lets LibreHardwareMonitor
// load its kernel driver and read the on-die sensor. When NOT elevated (e.g.
// `watchdesk run` in a normal shell) the driver can't load and readings come
// back as 0; we treat any non-positive value as unavailable and emit null, so
// a bogus 0 C is never reported for a running CPU.
//
// Built from source at compile time by build.rs using the in-box .NET Framework
// C# compiler (csc.exe) — no .NET SDK required.
//
// Args: [intervalSeconds=15] [sampleCount=0(=run forever)]
using System;
using System.Globalization;
using System.Linq;
using System.Threading;
using LibreHardwareMonitor.Hardware;

class Program
{
    static int Main(string[] args)
    {
        int intervalMs = 15000;
        int s;
        if (args.Length > 0 && int.TryParse(args[0], out s) && s > 0) intervalMs = s * 1000;
        int count = 0; // 0 = run forever
        if (args.Length > 1) int.TryParse(args[1], out count);

        var computer = new Computer { IsCpuEnabled = true };
        try
        {
            computer.Open();
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine("open failed: " + ex.Message);
            return 1;
        }

        int i = 0;
        while (count == 0 || i < count)
        {
            float? temp = ReadCpuTemp(computer);
            string val = temp.HasValue
                ? temp.Value.ToString("F1", CultureInfo.InvariantCulture)
                : "null";
            Console.WriteLine("{\"cpu_temp_c\":" + val + "}");
            Console.Out.Flush();
            i++;
            if (count == 0 || i < count) Thread.Sleep(intervalMs);
        }

        computer.Close();
        return 0;
    }

    // Prefer the main control/package temp (Tctl/Tdie); fall back to any CPU temp
    // sensor. A value must be > 0 to count as real (0 means the driver couldn't
    // read it, e.g. when not elevated).
    static float? ReadCpuTemp(Computer computer)
    {
        foreach (var hw in computer.Hardware)
        {
            if (hw.HardwareType != HardwareType.Cpu) continue;
            hw.Update();

            var main = hw.Sensors.FirstOrDefault(x =>
                x.SensorType == SensorType.Temperature &&
                x.Name.IndexOf("Tctl", StringComparison.OrdinalIgnoreCase) >= 0 &&
                x.Value.HasValue && x.Value.Value > 0f);
            if (main != null) return main.Value;

            var any = hw.Sensors.FirstOrDefault(x =>
                x.SensorType == SensorType.Temperature &&
                x.Value.HasValue && x.Value.Value > 0f);
            if (any != null) return any.Value;
        }
        return null;
    }
}
