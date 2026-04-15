#[macro_use]
extern crate log;
extern crate byteorder;
extern crate flate2;

//all files except this one came from another, this is the only file mainly written by author

mod device;
mod framebuffer;
#[macro_use]
mod geom;
mod color;
mod input;
mod security;
mod settings;
mod vnc;

pub use crate::framebuffer::image::ReadonlyPixmap;
use crate::framebuffer::{Framebuffer, KoboFramebuffer1, KoboFramebuffer2, Pixmap, UpdateMode};
use crate::geom::Rectangle;
use crate::vnc::{client, Client, Encoding, Rect};
use clap::{value_t, App, Arg};
use log::{debug, error, info};
use std::thread;
use std::time::Duration;
use std::time::Instant;
use vnc::PixelFormat;
use input::{raw_events, device_events, usb_events, display_rotate_event, button_scheme_event, DeviceEvent, FingerStatus};

use anyhow::{Context as ResultExt, Error};

use crate::device::CURRENT_DEVICE;
use std::fs::File;
use std::io::Read;
use std::mem;
use std::slice;
//use std::thread;
use std::path::Path;
use std::sync::mpsc;

const FB_DEVICE: &str = "/dev/fb0";
const TOUCH_INPUTS: [&str; 5] = ["/dev/input/by-path/platform-2-0010-event",
    "/dev/input/by-path/platform-1-0038-event",
    "/dev/input/by-path/platform-1-0010-event",
    "/dev/input/by-path/platform-0-0010-event",
    "/dev/input/event1"];
// //array of 5 items, string slice type
// const BUTTON_INPUTS: [&str; 4] = ["/dev/input/by-path/platform-gpio-keys-event",
//     "/dev/input/by-path/platform-ntx_event0-event",
//     "/dev/input/by-path/platform-mxckpd-event",
//     "/dev/input/event0"];
// const POWER_INPUTS: [&str; 3] = ["/dev/input/by-path/platform-bd71828-pwrkey.6.auto-event",
//     "/dev/input/by-path/platform-bd71828-pwrkey.4.auto-event",
//     "/dev/input/by-path/platform-bd71828-pwrkey-event"];
// const EV_SYN: u16 = 0x00;
// const EV_KEY: u16 = 0x01;
// const EV_ABS: u16 = 0x03;
//
// // SYN codes
// const SYN_REPORT: u16 = 0;
//
// // ABS codes
// const ABS_X: u16 = 0x00;
// const ABS_Y: u16 = 0x01;
// const ABS_MT_POSITION_X: u16 = 0x36;
// const ABS_MT_POSITION_Y: u16 = 0x35;
//
// // KEY codes
// const TOUCH: u16 = 0x14a;
// const BTN2: u16 = 0x102;
// #[repr(C)]
// #[derive(Debug, Copy, Clone)]
// struct TimeVal {
//     tv_sec: i32,
//     tv_usec: i32,
// }
//
// #[repr(C)]
// #[derive(Debug, Copy, Clone)]
// struct InputEvent {
//     time: TimeVal,
//     type_: u16,
//     code: u16,
//     value: i32,
// }
#[repr(align(256))]
pub struct PostProcBin {
    data: [u8; 256], //array of 256 values of u8, postprocessingbin?
}

fn main() -> Result<(), Error> {
    eprintln!("BEGIN");
    env_logger::init();

    let matches = App::new("einkvnc")
        .about("VNC client")
        .arg(
            Arg::with_name("HOST")
                .help("server hostname or IP")
                .required(true)
                .index(1),
        )
        .arg(
            Arg::with_name("PORT")
                .help("server port (default: 5900)")
                .index(2),
        )
        .arg(
            Arg::with_name("USERNAME")
                .help("server username")
                .long("username")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("PASSWORD")
                .help("server password")
                .long("password")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("EXCLUSIVE")
                .help("request a non-shared session")
                .long("exclusive"),
        )
        .arg(
            Arg::with_name("CONTRAST")
                .help("apply a post processing contrast filter")
                .long("contrast")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("GRAYPOINT")
                .help("the gray point of the post processing contrast filter")
                .long("graypoint")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("WHITECUTOFF")
                .help("apply a post processing filter to turn colors greater than the specified value to white (255)")
                .long("whitecutoff")
                .takes_value(true),
        ).arg(
        Arg::with_name("ROTATE")
            .help("rotation (1-4), tested on a Clara HD, try at own risk")
            .long("rotate")
            .takes_value(true),
        ).arg(
        Arg::with_name("SCALE")
            .help("scale to fit height or width")
            .long("scale")
            .takes_value(false),
        )
        .get_matches();

    let host = matches.value_of("HOST").unwrap();
    let port = value_t!(matches.value_of("PORT"), u16).unwrap_or(5900);
    let username = matches.value_of("USERNAME");
    let password = matches.value_of("PASSWORD");
    let contrast_exp = value_t!(matches.value_of("CONTRAST"), f32).unwrap_or(1.0);
    let contrast_gray_point = value_t!(matches.value_of("GRAYPOINT"), f32).unwrap_or(224.0);
    let white_cutoff = value_t!(matches.value_of("WHITECUTOFF"), u8).unwrap_or(255);
    let exclusive = matches.is_present("EXCLUSIVE");
    let rotate = value_t!(matches.value_of("ROTATE"), i8).unwrap_or(CURRENT_DEVICE.startup_rotation());
    let scale = matches.is_present("SCALE");

    info!("connecting to {}:{}", host, port);
    let stream = match std::net::TcpStream::connect((host, port)) {
        Ok(stream) => stream,
        Err(error) => {
            error!("cannot connect to {}:{}: {}", host, port, error);
            std::process::exit(1)
        }
    };//from crate std module net-TcpStream struct with method connect,
    // returns ok or error

    let mut vnc = match Client::from_tcp_stream(stream, !exclusive, |methods| {
        debug!("available authentication methods: {:?}", methods);
        //debug! macro is part of closure
        //mut ensures can later change the instance fields
        //|methods| is the input parameter for the closure, reference to AuthMethod enum?
        // auth is defined parameter input too, type Auth trait bound,
        // must support AuthMethod input AuthChoice output
        //stream is the tcp stream, from_tcp_stream is an associated function which returns client struct instance
        // 1st parameter type tcpstream, second boolean, 3rd auth, which is a closure returning option authchoice?
        //CHATGPT auth is a parameter variable.
        //
        // What Exactly Is auth?
        // Auth → a generic type parameter
        // auth → a value of type Auth
        // Auth must implement:
        // FnOnce(&[AuthMethod]) -> Option<AuthChoice>
        // So auth is:
        // A variable that holds a closure (or function) that can be called once.
        //auth is a value that implements FnOnce, meaning:
        // It is a closure (or function)
        // It takes &[AuthMethod]
        // It returns Option<AuthChoice>
        // So auth behaves like a function.

        //debug macro or trait?
        for method in methods { //methods input parameter from above function
            match method {
                client::AuthMethod::None => return Some(client::AuthChoice::None),
                client::AuthMethod::Password => {
                    return match password {
                        None => None,
                        Some(ref password) => {
                            let mut key = [0; 8];
                            for (i, byte) in password.bytes().enumerate() {
                                if i == 8 {
                                    break;
                                }
                                key[i] = byte
                            }
                            Some(client::AuthChoice::Password(key))
                        }
                    }
                }
                client::AuthMethod::AppleRemoteDesktop => match (username, password) {
                    (Some(username), Some(password)) => {
                        return Some(client::AuthChoice::AppleRemoteDesktop(
                            username.to_owned(),
                            password.to_owned(),
                        ))
                    }
                    _ => (),
                },
            }
        }
        None
    }) {
        //closure ends, function call ends
        Ok(vnc) => vnc,
        Err(error) => {
            error!("cannot initialize VNC session: {}", error);
            std::process::exit(1)
        }
    };

    /*let (tx, rx) = mpsc::channel();
    for path in TOUCH_INPUTS.iter() {
        let tx_clone = tx.clone();
        let mut file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("Skipping missing input: {}", path);
                continue;
            }
            Err(e) => {
                eprintln!("Failed to open {}: {}", path, e);
                continue;
            }
        };
        //let mut file = File::open(path)?;
        //open as file
        let mut event = InputEvent {
            time: TimeVal { tv_sec: 0, tv_usec: 0 },
            type_: 0,
            code: 0,
            value: 0,
        };

        thread::spawn(move || {
            let mut touch_x = 0u16;
            let mut touch_y = 0u16;
            loop {
                let buf = unsafe {
                    slice::from_raw_parts_mut(
                        &mut event as *mut InputEvent as *mut u8,
                        mem::size_of::<InputEvent>(),
                    )
                };

                file.read_exact(buf);

                eprintln!(
                    "txtype: {:04x} code: {:04x} value: {}", //pad with zeros, make 4 characters long min?
                    event.type_, event.code, event.value
                );
                match event.type_ {
                    EV_SYN => tx_clone.send((touch_x, touch_y)).unwrap(),
                    EV_KEY => match event.code {
                        TOUCH => continue,
                        BTN2 => continue,
                        _ => continue,
                    },
                    EV_ABS => match event.code {
                        ABS_MT_POSITION_X => touch_x = event.value as u16,
                        ABS_MT_POSITION_Y => touch_y = event.value as u16,
                        _ => continue,
                    },
                    _ => continue,
                }
            };
        });
    };*/

    let (width, height) = vnc.size();//tuple? vnc client struct? size method returns tuple. Info comes from
    //instance of Client struct returned by earlier function, which itself from the tcpstream
    info!(
        "connected to \"{}\", {}x{} framebuffer",
        vnc.name(),
        width,
        height
    );

    let vnc_format = vnc.format();//method returns PixelFormat struct
    info!("received {:?}", vnc_format);

    vnc.set_encodings(&[Encoding::CopyRect, Encoding::Zrle])
        .unwrap();
    //set encoding function method returns result, parameters are slice type of array?
    //unwrapis a method on Option and Result types that either
    // returns the value inside or panics if there is none (or an error).
    // It’s a way to get the value quickly
    // but is unsafe for production if you’re not certain the value exists.
    //client struct, impl methods set encoding, not trait but method, struct functions
    //mut self means can modify the instance of struct that has called the function
    //encodings: &[protocol::Encoding] parameter encodings of type slice reference

    vnc.request_update(//vnc client struct, req method returns result
                       // vnc instance of client struct passes values
                       Rect {
                           left: 0,
                           top: 0,
                           width,
                           height,
                       },
                       false,
    )
        .unwrap();

    #[cfg(feature = "eink_device")]
    //The debug! logging call is only compiled if the Cargo feature eink_device is enabled.
    debug!(
        "running on device model=\"{}\" /dpi={} /dims={}x{}",
        CURRENT_DEVICE.model,
        CURRENT_DEVICE.dpi,
        CURRENT_DEVICE.dims.0,
        CURRENT_DEVICE.dims.1
    );

    let mut fb: Box<dyn Framebuffer> = if CURRENT_DEVICE.mark() != 8 {
        //mutable variable fb, type Box pointer dynamic dispatch Framebuffer trait?
        // type box pointer? dynamic typing? pub trait framebuffer?
        Box::new(
            KoboFramebuffer1::new(FB_DEVICE)//fb device is constant with path to actual dev/fb?
                .context("can't create framebuffer")
                .unwrap(),
        )//Box::new(...)
        // converts the concrete framebuffer into a heap-allocated trait object.

        //.unwrap() ensures the Result returned by new is successfully unwrapped (panics if error).
    } else {
        Box::new(
            KoboFramebuffer2::new(FB_DEVICE)
                .context("can't create framebuffer")
                .unwrap(),
        )
    };

    #[cfg(feature = "eink_device")]
    {
        let startup_rotation = rotate;//pass value from rotate to startup rotation varaible?
        fb.set_rotation(startup_rotation).ok(); //fb ref to framebuffer above,
        // simply returns turns a value? belongs to trait, marker trait? i see the actual
        // function is defined elsewhere...
    }

    let post_proc_bin = PostProcBin {//struct defined earlier in this file
        data: (0..=255) //iterator creates range 0 to 255?
            .map(|i| { // .map is a function belonging to iterator module?
                //calls closure function on each iterated object
                if contrast_exp == 1.0 { //contrast_exp is cli flag
                    i // return i?
                } else {
                    let gray = contrast_gray_point;//cgp is a input from cli tool?

                    let rem_gray = 255.0 - gray;//remaining gray level?
                    let inv_exponent = 1.0 / contrast_exp;//cli input

                    let raw_color = i as f32;//i originalyl not f32? u8
                    //i input from earlier, is the closure input?
                    if raw_color < gray {
                        (gray * (raw_color / gray).powf(contrast_exp)) as u8
                        //gray percentage? which is executed first? .powf or gray*?
                        // RC/G 1st .powf CE 2nd *gray 3rd
                        //RC/G is current value over chosen gray point, what is i exactly? a value from range 0 to 255 created earlier
                    } else if raw_color > gray {
                        (gray + rem_gray * ((raw_color - gray) / rem_gray).powf(inv_exponent)) as u8
                    } else {
                        gray as u8
                    }
                }
            })
            .map(|i| -> u8 {
                if i > white_cutoff { //from cli flag
                    255
                } else {
                    i
                }
            })
            .collect::<Vec<u8>>()
            .try_into()
            .unwrap(),
    };

    const FRAME_MS: u64 = 1000 / 30;

    const MAX_DIRTY_REFRESHES: usize = 500;

    let mut dirty_rects: Vec<Rectangle> = Vec::new();
    //Vec of type Rect struct of 2 points,
    // min max?
    let mut dirty_rects_since_refresh: Vec<Rectangle> = Vec::new();
    //same
    //Vec<_> tells Rust to collect into a vector, and
    // _ lets the compiler infer the type (Vec<i32>)
    let mut has_drawn_once = false;
    let mut dirty_update_count = 0;

    let mut time_at_last_draw = Instant::now();

    let fb_rect = rect![0, 0, width as i32, height as i32];

    let post_proc_enabled = contrast_exp != 1.0;

    let mut paths = Vec::new();
    for ti in &TOUCH_INPUTS {
        if Path::new(ti).exists() {
            paths.push(ti.to_string());
            break;
        }
    }
    // for bi in &BUTTON_INPUTS {
    //     if Path::new(bi).exists() {
    //         paths.push(bi.to_string());
    //         break;
    //     }
    // }
    // for pi in &POWER_INPUTS {
    //     if Path::new(pi).exists() {
    //         paths.push(pi.to_string());
    //         break;
    //     }
    // }

    let (raw_sender, raw_receiver) = raw_events(paths);
    let touch_screen = device_events(raw_receiver, rotate);
    //let usb_port = usb_events();

    let (tx, rx) = mpsc::channel();
    let tx2 = tx.clone();

    thread::spawn(move || {
        while let Ok(evt) = touch_screen.recv() {
            tx2.send(evt).ok();
        }
    });

    'running: loop { //loop named 'running
        let time_at_sol = Instant::now();
        if let Ok(evt) = rx.try_recv() {
            match evt {
                DeviceEvent::Finger { id, time, status, position }  => {
                    println!("main dev ev finger{:?} {:?} {:?} {:?}", id, status, position.x as u16,position.y as u16);
                    match id {
                        0 => {
                            match status {
                                FingerStatus::Up => {
                                    vnc.send_pointer_event(0x00, position.x as u16, position.y as u16).unwrap();
                                },
                                FingerStatus::Down => {
                                    vnc.send_pointer_event(0x01, position.x as u16, position.y as u16).unwrap();
                                },
                                FingerStatus::Motion => {
                                    vnc.send_pointer_event(0x01, position.x as u16, position.y as u16).unwrap();
                                },
                            }
                        },
                        1 => {
                            match status {
                                FingerStatus::Up => {
                                    vnc.send_pointer_event(0x00, position.x as u16, position.y as u16).unwrap();
                                },
                                FingerStatus::Down => {
                                    vnc.send_pointer_event(0x01, position.x as u16, position.y as u16).unwrap();
                                },
                                FingerStatus::Motion => {
                                    vnc.send_pointer_event(0x01, position.x as u16, position.y as u16).unwrap();
                                },
                            }
                        },
                        2 => {
                            match status {
                                FingerStatus::Up => {
                                    vnc.send_pointer_event(0x00, position.x as u16, position.y as u16).unwrap();
                                },
                                FingerStatus::Down => {
                                    vnc.send_pointer_event(0x01, position.x as u16, position.y as u16).unwrap();
                                },
                                FingerStatus::Motion => {
                                    vnc.send_pointer_event(0x01, position.x as u16, position.y as u16).unwrap();
                                },
                            }
                        },
                        _ => {
                            eprintln!("Unknown finger ID")
                        },
                    }
                },
                // DeviceEvent::Button => {
                // },
                // DeviceEvent::RotateScreen(i8) => {
                // },
                _ => {}
            };
        };

        //thread is always listening but message will not be received until loop comes back around to here after
        //going through vnc events... so there will be a delay of touch input...
        //better if the touch input vnc sending is... immediate but would need to use arc?


        //get current time?
        //let event = rx.recv().unwrap();

        //eprintln!("B4");
        /*if let Ok(touch_event) = rx.try_recv() {
            eprintln!("{},{}",touch_event.0, touch_event.1);
            match rotate {
                0 => {
                    vnc.send_pointer_event(0x01, touch_event.0, touch_event.1).unwrap();
                    vnc.send_pointer_event(0x00, touch_event.0, touch_event.1).unwrap();
                },
                1 => {
                    vnc.send_pointer_event(0x01, touch_event.0, CURRENT_DEVICE.dims.1 as u16-touch_event.1).unwrap();
                    vnc.send_pointer_event(0x00, touch_event.0, CURRENT_DEVICE.dims.1 as u16-touch_event.1).unwrap();
                },
                2 => {
                    vnc.send_pointer_event(0x01, CURRENT_DEVICE.dims.0 as u16-touch_event.0, CURRENT_DEVICE.dims.1 as u16-touch_event.1).unwrap();
                    vnc.send_pointer_event(0x00, CURRENT_DEVICE.dims.0 as u16-touch_event.0, CURRENT_DEVICE.dims.1 as u16-touch_event.1).unwrap();
                },
                3 => {
                    vnc.send_pointer_event(0x01, CURRENT_DEVICE.dims.0 as u16-touch_event.0, touch_event.1).unwrap();
                    vnc.send_pointer_event(0x00, CURRENT_DEVICE.dims.0 as u16-touch_event.0, touch_event.1).unwrap();
                },
                _ => continue
            }//framebyffer origin top left, touch right corner
            //when landscaped, touch origin remains same, but fb origin axes changes?
            //0 is left, 2 is right rotate. so touch input alwasy the same, but when send to
            //vnc server, must rotate or invert it because touch axes remained the same

            //eprintln!("TouchLoop");
        }*/
        for event in vnc.poll_iter() {//poll iter is a method belonging to client trait, implemented
            //as return a client struct? or wat?
            use client::Event;//

            //for vnc struct of client type? Event is an enum? ah okay were only iterting the events field?
            //vnc takes parameters from the tcp function as its fields values?
            match event { //match event in the iterated vnc struct fields?
                Event::Disconnected(None) => break 'running, //event enum!
                Event::Disconnected(Some(error)) => {
                    error!("server disconnected: {:?}", error);
                    break 'running;
                }
                //I see, vnc_Rect and ref pixels are bound to the match from event in vnc.poll_iter
                Event::PutPixels(vnc_rect, ref pixels) => {
                    debug!("Put pixels");

                    let elapsed_ms = time_at_sol.elapsed().as_millis();
                    debug!("network Δt: {}", elapsed_ms);

                    let scale_down =
                        pixels
                            .iter()
                            .step_by(4) //every 4th byte is Red value of pixel?
                            .map(|&c| post_proc_bin.data[c as usize])
                            .collect();
                    //iterate through received pixels from vnc poll iter and step size 4, so every
                    //4th pixel is collected into a vector. scale down, w no average?

                    let post_proc_pixels = if post_proc_enabled {
                        pixels
                            .iter()
                            .step_by(4)
                            .map(|&c| post_proc_bin.data[c as usize])
                            .collect()
                    } else {
                        Vec::new()
                    };
                    //postprocessing pixels again, ref to c? closure
                    //post proc bin is a look up table? there are many c variables, which does it refer to?
                    //is it password c or u8 c ?

                    let pixels = if post_proc_enabled {
                        &post_proc_pixels
                    } else {
                        &scale_down
                    };

                    let w = vnc_rect.width as u32;
                    let h = vnc_rect.height as u32;
                    let l = vnc_rect.left as u32;
                    let t = vnc_rect.top as u32; //partial image of fb

                    let pixmap = ReadonlyPixmap {
                        width: w as u32,
                        height: h as u32,
                        data: pixels, //entire frame?
                    };
                    debug!("Put pixels {} {} {} size {}",w,h,w*h,pixels.len());

                    let elapsed_ms = time_at_sol.elapsed().as_millis();
                    debug!("postproc Δt: {}", elapsed_ms);

                    #[cfg(feature = "eink_device")]
                    {
                        for y in 0..pixmap.height {
                            for x in 0..pixmap.width {
                                let px = x + l;
                                let py = y + t;
                                let color = pixmap.get_pixel(x, y);
                                fb.set_pixel(px, py, color);
                            }
                        } //loop through all rect co ords to map onto full frame location
                    }

                    let elapsed_ms = time_at_sol.elapsed().as_millis();
                    debug!("draw Δt: {}", elapsed_ms);

                    let w = vnc_rect.width as i32; //earlier u32 now i32, why?
                    let h = vnc_rect.height as i32;
                    let l = vnc_rect.left as i32;
                    let t = vnc_rect.top as i32;

                    let delta_rect = rect![l, t, l + w, t + h];
                    //new rect using those co ords?
                    if delta_rect == fb_rect {
                        //if  let fb_rect = rect![0, 0, width as i32, height as i32];
                        //oh checks if rect is the size of entire framebuffer?
                        dirty_rects.clear(); //remove all vector values
                        dirty_rects_since_refresh.clear();
                        #[cfg(feature = "eink_device")]
                        {
                            if !has_drawn_once || dirty_update_count > MAX_DIRTY_REFRESHES {
                                //if false or more than, initialised as false first so will be triggered first pass
                                fb.update(&fb_rect, UpdateMode::Full).ok();//fb_rect is entire fb
                                dirty_update_count = 0;
                                //reset, below has counter to increment each endofframe
                                has_drawn_once = true;//make true now so next time wont trigger
                            } else {
                                fb.update(&fb_rect, UpdateMode::Partial).ok();//so the first draw
                                //is always full gc16, next ones are partial, then after set amount full
                            }
                        }
                    } else {
                        push_to_dirty_rect_list(&mut dirty_rects, delta_rect);
                        //if rect is smaller than fb, track dirty rects?
                    }

                    let elapsed_ms = time_at_sol.elapsed().as_millis();
                    debug!("rects Δt: {}", elapsed_ms);
                }
                Event::CopyPixels { src, dst } => {
                    debug!("Copy pixels!");

                    #[cfg(feature = "eink_device")]
                    {
                        let src_left = src.left as u32;
                        let src_top = src.top as u32;

                        let dst_left = dst.left as u32;
                        let dst_top = dst.top as u32;
                        //which rects to copy from one place to another

                        let mut intermediary_pixmap =
                            Pixmap::new(dst.width as u32, dst.height as u32);

                        for y in 0..intermediary_pixmap.height {
                            for x in 0..intermediary_pixmap.width {
                                let color = fb.get_pixel(src_left + x, src_top + y);
                                intermediary_pixmap.set_pixel(x, y, color);
                            }
                        }//get pix value from framebuffer, put onto intermediary

                        for y in 0..intermediary_pixmap.height {
                            for x in 0..intermediary_pixmap.width {
                                let color = intermediary_pixmap.get_pixel(x, y);
                                fb.set_pixel(dst_left + x, dst_top + y, color);
                            }//get pix from intermediary, put back onto fb
                        }
                    }

                    let delta_rect = rect![
                        dst.left as i32,
                        dst.top as i32,
                        (dst.left + dst.width) as i32,
                        (dst.top + dst.height) as i32
                    ];//new rect struct from destination?
                    push_to_dirty_rect_list(&mut dirty_rects, delta_rect);//add to dr list for updating?
                    //but doesnt fb.set pix arleady update?
                }
                Event::EndOfFrame => {
                    debug!("End of frame!");

                    if !has_drawn_once {
                        has_drawn_once = dirty_rects.len() > 0;
                        //1st pass false, thus triggered. if dirty rects bigger than 0,
                        //make it true
                    }

                    dirty_update_count += 1;

                    if dirty_update_count > MAX_DIRTY_REFRESHES {
                        info!("Full refresh!");
                        for dr in &dirty_rects_since_refresh {
                            #[cfg(feature = "eink_device")]
                            {
                                fb.update(&dr, UpdateMode::Full).ok();
                            }
                        }
                        dirty_update_count = 0;
                        dirty_rects_since_refresh.clear();
                    } else {
                        for dr in &dirty_rects {
                            debug!("Updating dirty rect {:?}", dr);

                            #[cfg(feature = "eink_device")]
                            {
                                if dr.height() < 100 && dr.width() < 100 {
                                    debug!("Fast mono update!");
                                    fb.update(&dr, UpdateMode::FastMono).ok();
                                } else {
                                    fb.update(&dr, UpdateMode::Partial).ok();
                                }
                            }

                            push_to_dirty_rect_list(&mut dirty_rects_since_refresh, *dr);
                        }

                        time_at_last_draw = Instant::now();
                    }

                    dirty_rects.clear();
                }
                /*Event::SetCursor => {

            }*/
                // x => info!("{:?}", x), /* ignore unsupported events */
                _ => (),

            }
        }

        if FRAME_MS > time_at_sol.elapsed().as_millis() as u64 {
            if dirty_rects_since_refresh.len() > 0 && time_at_last_draw.elapsed().as_secs() > 3 {
                for dr in &dirty_rects_since_refresh {
                    #[cfg(feature = "eink_device")]
                    {
                        fb.update(&dr, UpdateMode::Full).ok();
                    }
                }
                dirty_update_count = 0;
                dirty_rects_since_refresh.clear();
            }

            if FRAME_MS > time_at_sol.elapsed().as_millis() as u64 {
                thread::sleep(Duration::from_millis(
                    FRAME_MS - time_at_sol.elapsed().as_millis() as u64,
                ));
            }
        } else {
            info!(
                "Missed frame, excess Δt: {}ms",
                time_at_sol.elapsed().as_millis() as u64 - FRAME_MS
            );
        }

        if vnc.request_update(
            Rect {
                left: 0,
                top: 0,
                width,
                height,
            },
            true,
        ).is_err() {
            error!("server disconnected");
            break;
        }
    }

    Ok(())
}

fn push_to_dirty_rect_list(list: &mut Vec<Rectangle>, rect: Rectangle) {
    for dr in list.iter_mut() {
        if dr.contains(&rect) {
            return;
        }
        if rect.contains(&dr) {
            *dr = rect;
            return;
        }
        if rect.extends(&dr) {
            dr.absorb(&rect);
            return;
        }
    }

    list.push(rect);
}
