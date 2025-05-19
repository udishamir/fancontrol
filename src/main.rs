/*
 * fancontrol - ASUS Strix Liquid Cooling PWM Controller for Linux
 *
 * Copyright (c) 2025 Udi Shamir
 * SPDX-License-Identifier: MIT
 *
 * This program provides fine-grained fan control for ASUS Strix motherboards and AIOs
 * under Linux. ASUS exposes fan control primarily through a proprietary WMI interface
 * on Windows, leaving Linux users without native support for liquid cooling tuning.
 *
 * By leveraging the nct6775 kernel module and sysfs interface, this tool enables:
 *   - Manual PWM fan control
 *   - Configuration via "fancontrol.toml"
 *   - Real-time inspection of current fan RPM and PWM state
 *
 * This tool was created to bypass vendor lock-in and reclaim control over hardware
 * that is otherwise inaccessible on non-Windows platforms.
 *
 * Dependencies:
 *   - Kernel module: "nct6775" (must be loaded via "modprobe nct6775""
 *   - System: sysfs interface (/sys/class/hwmon)
 *
 * Licensed under the MIT License. See LICENSE for details.
*/

use clap::{Parser, Subcommand};
use std::fs;
use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

const HWMON_PATH: &str = "/sys/class/hwmon";
const K10TEMP_SENSOR_NAME: &str = "k10temp";
/*
    Nuvoton support is essential
    https://www.nuvoton.com/resource-files/NCT6796D_Datasheet_V0_6.pdf
    https://docs.kernel.org/hwmon/nct6775.html
    https://www.phoronix.com/news/Linux-6.4-nct6775-More-ASUS
*/
const SENSOR_CANDIDATES: &[&str] = &["nct6799", "nct6775", "nct7802", "as99127f"];

#[derive(Parser)]
#[command(name = "fancontrol")]
#[command(about = "Rust CLI utility for temperature and fan control")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Temp,
    ListFans,
    ListPwm,
    SetPwm {
        pwm_index: u8,
        value: u8,
    },
    SetMode {
        pwm_index: u8,
        // Can be set to manual or auto, auto is the default BIOS settings
        mode: String,
    },
    Daemon {
        #[arg(short, long, default_value_t = 1)]
        pwm_index: u8,
    },
}

fn check_module_loaded() -> bool {
    Path::new("/sys/module/nct6775").exists()
}

fn find_hwmon_path(sensor_name: &str) -> io::Result<String> {
    for entry in fs::read_dir(HWMON_PATH)? {
        // need to replace with match, this is not production
        let entry = entry?;
        let name_path = entry.path().join("name");
        if let Ok(name) = fs::read_to_string(&name_path) {
            if name.trim() == sensor_name {
                return Ok(entry.path().to_string_lossy().into());
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("{} sensor not found", sensor_name),
    ))
}

fn find_hwmon_path_dynamic() -> io::Result<String> {
    for entry in fs::read_dir(HWMON_PATH)? {
        let entry = entry?;
        let name_path = entry.path().join("name");
        if let Ok(name) = fs::read_to_string(&name_path) {
            if SENSOR_CANDIDATES.iter().any(|&s| s == name.trim()) {
                return Ok(entry.path().to_string_lossy().into());
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "No supported sensor found",
    ))
}

fn read_cpu_temperature() -> io::Result<f32> {
    let path = find_hwmon_path(K10TEMP_SENSOR_NAME)?;
    let temp_path = Path::new(&path).join("temp1_input");
    let raw = fs::read_to_string(temp_path)?;
    let milli_degrees: i32 = raw
        .trim()
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(milli_degrees as f32 / 1000.0)
}

fn list_fans() -> io::Result<()> {
    let path = find_hwmon_path_dynamic()?;
    for i in 1..=7 {
        let fan_path = Path::new(&path).join(format!("fan{}_input", i));
        if fan_path.exists() {
            let val = fs::read_to_string(fan_path)?.trim().to_string();
            println!("Fan{}: {} RPM", i, val);
        }
    }
    Ok(())
}

fn list_pwm() -> io::Result<()> {
    let path = find_hwmon_path_dynamic()?;
    for i in 1..=7 {
        let pwm_path = Path::new(&path).join(format!("pwm{}", i));
        let enable_path = Path::new(&path).join(format!("pwm{}_enable", i));
        let max_path = Path::new(&path).join(format!("pwm{}_max", i));

        if pwm_path.exists() && enable_path.exists() {
            let val: u8 = fs::read_to_string(&pwm_path)?.trim().parse().unwrap_or(0);
            let mode = match fs::read_to_string(&enable_path)?.trim() {
                "1" => "manual",
                "2" => "auto",
                _ => "unknown",
            };
            let max_val: u8 = if max_path.exists() {
                fs::read_to_string(&max_path)?.trim().parse().unwrap_or(255)
            } else {
                255
            };
            let percent = (val as f32 / max_val as f32) * 100.0;
            println!("PWM{}: value={}, ~{:.1}%, mode={}", i, val, percent, mode);
        }
    }
    Ok(())
}

fn set_pwm(pwm_index: u8, value: u8) -> io::Result<()> {
    let path = find_hwmon_path_dynamic()?;
    let enable_path = Path::new(&path).join(format!("pwm{}_enable", pwm_index));
    let pwm_path = Path::new(&path).join(format!("pwm{}", pwm_index));
    let max_path = Path::new(&path).join(format!("pwm{}_max", pwm_index));

    fs::write(&enable_path, b"1")?;
    fs::write(&pwm_path, format!("{}", value))?;

    let max_val: u8 = if max_path.exists() {
        fs::read_to_string(&max_path)?.trim().parse().unwrap_or(255)
    } else {
        255
    };

    let percent = (value as f32 / max_val as f32) * 100.0;
    println!("Set pwm{} to {} (~{:.1}%)", pwm_index, value, percent);
    Ok(())
}

fn set_mode(pwm_index: u8, mode: &str) -> io::Result<()> {
    let path = find_hwmon_path_dynamic()?;
    let enable_path = Path::new(&path).join(format!("pwm{}_enable", pwm_index));
    let mode_val = match mode {
        "manual" => "1",
        "auto" => "2",
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "mode must be 'manual' or 'auto'",
            ));
        }
    };
    fs::write(enable_path, mode_val)?;
    println!("Set pwm{} mode to {}", pwm_index, mode);
    Ok(())
}

fn temp_to_pwm(temp_c: f32) -> u8 {
    match temp_c {
        t if t <= 40.0 => 80,
        t if t <= 50.0 => 128,
        t if t <= 60.0 => 180,
        _ => 255,
    }
}

fn run_daemon(pwm_index: u8) -> io::Result<()> {
    println!("Starting fan control daemon on pwm{}", pwm_index);
    loop {
        let temp = read_cpu_temperature()?;
        let pwm = temp_to_pwm(temp);
        set_pwm(pwm_index, pwm)?;
        thread::sleep(Duration::from_secs(5));
    }
}

fn main() -> io::Result<()> {
    if !check_module_loaded() {
        println!("Warning: 'nct6775' kernel module is not loaded.");
        println!("Run: sudo modprobe nct6775");
    }

    let cli = Cli::parse();

    match &cli.command {
        Commands::Temp => {
            let temp_c = read_cpu_temperature()?;
            println!("Current CPU Temperature: {:.1} °C", temp_c);
        }
        Commands::ListFans => {
            list_fans()?;
        }
        Commands::ListPwm => {
            list_pwm()?;
        }
        Commands::SetPwm { pwm_index, value } => {
            set_pwm(*pwm_index, *value)?;
        }
        Commands::SetMode { pwm_index, mode } => {
            set_mode(*pwm_index, mode)?;
        }
        Commands::Daemon { pwm_index } => {
            run_daemon(*pwm_index)?;
        }
    }

    Ok(())
}
