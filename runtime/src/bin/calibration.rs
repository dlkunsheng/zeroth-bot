use ctrlc;
use runtime::hal::Servo;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;
use std::env;
use anyhow::{Result, bail};

const CURRENT_THRESHOLD: f32 = 500.0; // mA
const CALIBRATION_SPEED: u16 = 250;
const MIN_SPEED: u16 = 10;

const SERVO_ADDR_EEPROM_WRITE: u8 = 0x37;
const SERVO_ADDR_POSITION_CORRECTION: u8 = 0x1F;
const SERVO_ADDR_MIN_ANGLE: u8 = 0x09;
const SERVO_ADDR_MAX_ANGLE: u8 = 0x0B;
const SERVO_ADDR_OPERATION_MODE: u8 = 0x21;
const SERVO_ADDR_TARGET_POSITION: u8 = 0x2A;

pub fn calibrate_servo(servo: &Servo, servo_id: u8, running: &Arc<AtomicBool>) -> Result<()> {
    println!("Starting servo calibration for ID: {}", servo_id);

    servo.disable_readout()?;
    servo.set_mode(servo_id, 1)?; // Set to continuous mode

    let mut max_forward = 0;
    let mut max_backward = 0;

    for pass in 0..2 {
        let direction = if pass == 0 { 1 } else { -1 };
        println!(
            "Starting calibration pass {}, direction: {}",
            pass + 1,
            if direction == 1 {
                "forward"
            } else {
                "backward"
            }
        );

        servo.set_speed(servo_id, CALIBRATION_SPEED, direction)?;

        loop {
            if !running.load(Ordering::SeqCst) {
                println!("Calibration interrupted. Stopping servo...");
                servo.set_speed(servo_id, 0, 1)?;
                return Ok(());
            }

            let info = servo.read_info(servo_id)?;
            let position = info.current_location;
            let mut current = info.current_current as f32 * 6.5 / 100.0;

            if current > CURRENT_THRESHOLD {
                println!("Current threshold reached at position {}", position);

                // Stop
                servo.set_speed(servo_id, 0, direction)?;
                sleep(Duration::from_millis(100));

                println!("Backing off");
                // Back off
                servo.set_speed(servo_id, CALIBRATION_SPEED, -direction)?;
                sleep(Duration::from_millis(100));

                // Stop after backoff
                servo.set_speed(servo_id, 0, -direction)?;
                sleep(Duration::from_millis(100));
                println!("Backing off complete");
                // Move slowly to find exact position
                servo.set_speed(servo_id, MIN_SPEED, direction)?;
                while current <= CURRENT_THRESHOLD * 2.0 {
                    if !running.load(Ordering::SeqCst) {
                        println!("Calibration interrupted. Stopping servo...");
                        servo.set_speed(servo_id, 0, 1)?;
                        return Ok(());
                    }

                    let info = servo.read_info(servo_id)?;
                    current = info.current_current as f32 * 6.5 / 100.0;
                    sleep(Duration::from_millis(10));
                }

                // Stop at exact position
                servo.set_speed(servo_id, 0, direction)?;
                sleep(Duration::from_millis(100));

                let info = servo.read_info(servo_id)?;
                println!("Exact threshold position found: {}", info.current_location);

                // Back off again
                servo.set_speed(servo_id, CALIBRATION_SPEED, -direction)?;
                sleep(Duration::from_millis(100));

                // Stop after final backoff
                servo.set_speed(servo_id, 0, -direction)?;
                sleep(Duration::from_millis(100));

                let info = servo.read_info(servo_id)?;
                println!(
                    "Calibration complete for this direction. Final position: {}",
                    info.current_location
                );

                if direction == 1 {
                    max_forward = info.current_location;
                    println!(
                        "Forward calibration complete. Max position: {}",
                        max_forward
                    );
                } else {
                    max_backward = info.current_location;
                    println!(
                        "Backward calibration complete. Max position: {}",
                        max_backward
                    );
                }

                break;
            }

            sleep(Duration::from_millis(10));
        }

        if pass < 1 {
            println!("Changing direction for next calibration pass...");
            sleep(Duration::from_millis(500));
        }
    }

    servo.set_speed(servo_id, CALIBRATION_SPEED, 1)?;
    sleep(Duration::from_millis(100));
    servo.set_speed(servo_id, 0, 1)?;

    // // Switch to servo mode (3)
    // servo.write(servo_id, SERVO_ADDR_OPERATION_MODE, &[3])?;
    // println!("Switched servo to mode 3.");

    // Ensure max_angle > min_angle
    let min_angle = max_backward;
    let mut max_angle = max_forward;
    if max_angle <= min_angle {
        max_angle += 4096;
    }

    let center_distance = (max_angle - min_angle) / 2;
    // Calculate offset
    let offset = min_angle + center_distance - 2048;

    // Convert offset to 12-bit signed value
    let offset_value = if offset < 0 {
        (offset & 0x7FF) as u16 | 0x800 // Set sign bit
    } else {
        (offset & 0x7FF) as u16
    };


    // unlock EEPROM
    servo.write(servo_id, SERVO_ADDR_EEPROM_WRITE, &[0])?;
    sleep(Duration::from_millis(10));

    servo.write(servo_id, SERVO_ADDR_OPERATION_MODE, &[0])?;
    sleep(Duration::from_millis(10));
    println!("Switched servo to mode 0.");

    write_servo_memory(
        &servo,
        servo_id,
        SERVO_ADDR_POSITION_CORRECTION,
        offset_value,
    )?;

    sleep(Duration::from_millis(10));
    // Write servo limits to memory
    write_servo_memory(&servo, servo_id, SERVO_ADDR_MIN_ANGLE, min_angle as u16)?;
    sleep(Duration::from_millis(10));
    write_servo_memory(&servo, servo_id, SERVO_ADDR_MAX_ANGLE, max_angle as u16)?;
    sleep(Duration::from_millis(10));
    // lock EEPROM
    servo.write(servo_id, SERVO_ADDR_EEPROM_WRITE, &[1])?;

    println!("Successfully wrote calibration data to EEPROM.");

    println!("Calibration complete.");
    println!("Offset: {}", offset);
    println!("Min Angle: {}", min_angle);
    println!("Max Angle: {}", max_angle);

    sleep(Duration::from_millis(100));

    let position_data = [(2048 & 0xFF) as u8, ((2048 >> 8) & 0xFF) as u8];
    servo.write(servo_id, SERVO_ADDR_TARGET_POSITION, &position_data)?;

    println!("Wrote servo limits to memory:");
    println!("Min Angle: {}", min_angle);
    println!("Max Angle: {}", max_angle);

    println!("Moving servo to middle 2048");

    sleep(Duration::from_secs(1));

    println!("Calibration and positioning complete.");

    servo.enable_readout()?;

    // Disable torque
    let torque_data = 0u8;
    match servo.write(servo_id, 0x28, &[torque_data]) {
        Ok(_) => println!("Torque disabled successfully."),
        Err(e) => println!("Failed to disable torque. Error: {}", e),
    }
    Ok(())
}

fn write_servo_memory(servo: &Servo, id: u8, address: u8, value: u16) -> Result<()> {
    let data = [(value & 0xFF) as u8, ((value >> 8) & 0xFF) as u8];
    servo.write(id, address, &data)
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let servo_id = match args.get(1) {
        Some(arg) => arg.parse().map_err(|_| anyhow::anyhow!("Invalid servo ID"))?,
        None => bail!("Servo ID must be specified as a command-line argument"),
    };

    println!("Starting calibration for servo ID: {}", servo_id);

    let servo = Arc::new(Servo::new()?);
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        println!("\nInterrupt signal received. Stopping calibration...");
        r.store(false, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl-C handler");

    let result = calibrate_servo(&servo, servo_id, &running);

    if !running.load(Ordering::SeqCst) {
        println!("Calibration was interrupted. Cleaning up...");
        // Perform any necessary cleanup
        servo.set_speed(servo_id, 0, 1)?; // Stop the servo
        servo.enable_readout()?;
    }

    result
}
