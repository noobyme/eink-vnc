use std::mem::{self, MaybeUninit};
use std::ptr;
use std::slice;
use std::thread;
use std::io::Read;
use std::fs::File;
use std::sync::mpsc::{self, Sender, Receiver};
use std::os::unix::io::AsRawFd;
use std::ffi::CString;
use fxhash::FxHashMap;
use crate::framebuffer::Display;
use crate::settings::ButtonScheme;
use crate::device::CURRENT_DEVICE;
use crate::geom::{Point, LinearDir};
use anyhow::{Error, Context};

// Event types
pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_ABS: u16 = 0x03;
pub const EV_MSC: u16 = 0x04;

// Event codes
pub const ABS_MT_TRACKING_ID: u16 = 0x39;
pub const ABS_MT_POSITION_X: u16 = 0x35;
pub const ABS_MT_POSITION_Y: u16 = 0x36;
pub const ABS_MT_PRESSURE: u16 = 0x3a;
pub const ABS_MT_TOUCH_MAJOR: u16 = 0x30;
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_PRESSURE: u16 = 0x18;
pub const MSC_RAW: u16 = 0x03;
pub const SYN_REPORT: u16 = 0x00;

// Event values
pub const MSC_RAW_GSENSOR_PORTRAIT_DOWN: i32 = 0x17;
pub const MSC_RAW_GSENSOR_PORTRAIT_UP: i32 = 0x18;
pub const MSC_RAW_GSENSOR_LANDSCAPE_RIGHT: i32 = 0x19;
pub const MSC_RAW_GSENSOR_LANDSCAPE_LEFT: i32 = 0x1a;
// pub const MSC_RAW_GSENSOR_BACK: i32 = 0x1b;
// pub const MSC_RAW_GSENSOR_FRONT: i32 = 0x1c;

// The indices of this clockwise ordering of the sensor values match the Forma's rotation values.
pub const GYROSCOPE_ROTATIONS: [i32; 4] = [MSC_RAW_GSENSOR_LANDSCAPE_LEFT, MSC_RAW_GSENSOR_PORTRAIT_UP,
                                           MSC_RAW_GSENSOR_LANDSCAPE_RIGHT, MSC_RAW_GSENSOR_PORTRAIT_DOWN];

pub const VAL_RELEASE: i32 = 0;
pub const VAL_PRESS: i32 = 1;
pub const VAL_REPEAT: i32 = 2;

// Key codes
pub const KEY_POWER: u16 = 116;
pub const KEY_HOME: u16 = 102;
pub const KEY_LIGHT: u16 = 90;
pub const KEY_BACKWARD: u16 = 193;
pub const KEY_FORWARD: u16 = 194;
pub const PEN_ERASE: u16 = 331;
pub const PEN_HIGHLIGHT: u16 = 332;
pub const SLEEP_COVER: [u16; 2] = [59, 35];
// Synthetic touch button
pub const BTN_TOUCH: u16 = 330;
// The following key codes are fake, and are used to support
// software toggles within this design
pub const KEY_ROTATE_DISPLAY: u16 = 0xffff;
pub const KEY_BUTTON_SCHEME: u16 = 0xfffe;

pub const SINGLE_TOUCH_CODES: TouchCodes = TouchCodes {
    pressure: ABS_PRESSURE,
    x: ABS_X,
    y: ABS_Y,
};

pub const MULTI_TOUCH_CODES_A: TouchCodes = TouchCodes {
    pressure: ABS_MT_TOUCH_MAJOR,
    x: ABS_MT_POSITION_X,
    y: ABS_MT_POSITION_Y,
};

pub const MULTI_TOUCH_CODES_B: TouchCodes = TouchCodes {
    pressure: ABS_MT_PRESSURE,
    .. MULTI_TOUCH_CODES_A //fill rest of fields with this struct
};

#[repr(C)]
pub struct InputEvent {
    pub time: libc::timeval,
    pub kind: u16, // type
    pub code: u16,
    pub value: i32,
}

// Handle different touch protocols
#[derive(Debug)]
pub struct TouchCodes {
    pressure: u16,
    x: u16,
    y: u16,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum TouchProto {
    Single,
    MultiA,
    MultiB, // Pressure won't indicate a finger release.
    MultiC,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum FingerStatus {
    Down,
    Motion,
    Up,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ButtonStatus {
    Pressed,
    Released,
    Repeated,
}

impl ButtonStatus {
    pub fn try_from_raw(value: i32) -> Option<ButtonStatus> {
        match value {
            VAL_RELEASE => Some(ButtonStatus::Released),
            VAL_PRESS => Some(ButtonStatus::Pressed),
            VAL_REPEAT => Some(ButtonStatus::Repeated),
            _ => None,
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum ButtonCode {
    Power,
    Home,
    Light,
    Backward,
    Forward,
    Erase,
    Highlight,
    Raw(u16),
}

impl ButtonCode { //enum methods/associated functions
    fn from_raw(code: u16, rotation: i8, button_scheme: ButtonScheme) -> ButtonCode {
        match code {
            KEY_POWER => ButtonCode::Power,
            KEY_HOME => ButtonCode::Home,
            KEY_LIGHT => ButtonCode::Light,
            KEY_BACKWARD => resolve_button_direction(LinearDir::Backward, rotation, button_scheme),
            KEY_FORWARD => resolve_button_direction(LinearDir::Forward, rotation, button_scheme),
            PEN_ERASE => ButtonCode::Erase,
            PEN_HIGHLIGHT => ButtonCode::Highlight,
            _ => ButtonCode::Raw(code)
            //match constants defined above, returns buttoncode enum variant
        }
    }
}

fn resolve_button_direction(mut direction: LinearDir, rotation: i8, button_scheme: ButtonScheme) -> ButtonCode {
    if (CURRENT_DEVICE.should_invert_buttons(rotation)) ^ (button_scheme == ButtonScheme::Inverted) {
        direction = direction.opposite();
    }

    if direction == LinearDir::Forward {
        return ButtonCode::Forward;
    }

    ButtonCode::Backward
}

pub fn display_rotate_event(n: i8) -> InputEvent {
    let mut tp = libc::timeval { tv_sec: 0, tv_usec: 0 };
    unsafe { libc::gettimeofday(&mut tp, ptr::null_mut()); }
    InputEvent {
        time: tp,
        kind: EV_KEY,
        code: KEY_ROTATE_DISPLAY,
        value: n as i32,
    }
}//setting up touch event struct

pub fn button_scheme_event(v: i32) -> InputEvent {
    let mut tp = libc::timeval { tv_sec: 0, tv_usec: 0 };
    unsafe { libc::gettimeofday(&mut tp, ptr::null_mut()); }
    InputEvent {
        time: tp,
        kind: EV_KEY,
        code: KEY_BUTTON_SCHEME,
        value: v,
    }
}//setting up event struct

#[derive(Debug, Copy, Clone)]
pub enum DeviceEvent {
    Finger {
        id: i32,
        time: f64,
        status: FingerStatus,
        position: Point,
    },
    Button {
        time: f64,
        code: ButtonCode,
        status: ButtonStatus,
    },
    Plug(PowerSource),
    Unplug(PowerSource),
    RotateScreen(i8),
    CoverOn,
    CoverOff,
    NetUp,
    UserActivity,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PowerSource {
    Host,
    Wall,
}

pub fn seconds(time: libc::timeval) -> f64 {
    time.tv_sec as f64 + time.tv_usec as f64 / 1e6
}

pub fn raw_events(paths: Vec<String>) -> (Sender<InputEvent>, Receiver<InputEvent>) {
    //<T> generic type parameter so can handle whatever struct is actually initiated
    let (tx, rx) = mpsc::channel();
    //create a new channel
    let tx2 = tx.clone();
    //clone sender so can move one and return the other
    thread::spawn(move || parse_raw_events(&paths, &tx)); //sends message input_event.assume_init()
    //usually func creates new scope so channels are not seen by each function.
    //each channel creation is new, but here if pass as ref to closure andmove, can see?
    //i suppose in the main.rs function call you nest the function calls so one returns to the other as parameters
    (tx2, rx)//return the channel pair
}//create channel and thread, which calls the actual function that listens. returns sender and receiver
//parse raw is only for touch input
//for every channel function, call the actual function and move to thread
//raw events reads touch input codes, then returns the channels, clone is returned bc thread owns original?
//raw events reads touch codes, returns inputevent struct channel sender and receiver,
// device events recieves inputevent, returns deviceevent enum receiver,
// then usb events returns deviceevent receiver
//rust supports multi sender but only one receiver
pub fn device_events(rx: Receiver<InputEvent>, rotation:i8) -> Receiver<DeviceEvent> {
    //since only 1 receiver channel is possible, the input parameter for this function
    //is the receiver from raw_events, which will receive an inputevent struct
    let (ty, ry) = mpsc::channel();//new channel
    thread::spawn(move || parse_device_events(&rx, &ty, rotation));//thread spawn take ownership of sending channel
    //take ownership of receiving channel from rawevents too, then uses sending channel to send deviceeventenum receiver back to this function?
    ry//return the receiver channel, but we have not yet used it to receive any message, that will be done in main when actually call
    //these functions


    //no need clone bc only rx and ty are used?
    //device events translates the structs into actual touch
}
pub fn usb_events() -> Receiver<DeviceEvent> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || parse_usb_events(&tx));
    rx //returns another channel, a new one, not linked to the other. returns receiver so when main app calls it can recv a value but not yet here
    //no need clone bc only tx is used?
}

//below code shows how channels are created and then passed, but function parameter only defines the parameter and does
//not necessarily mean its the same one as created, can pass any into it. define an input parameter as same name as channel,
//shiould have used a diff name so not confusing.,..

//let (raw_sender, raw_receiver) = raw_events(paths);
//     let touch_screen = gesture_events(device_events(raw_receiver, context.display, context.settings.button_scheme));
//     let usb_port = usb_events();
//
//     let (tx, rx) = mpsc::channel();
//     let tx2 = tx.clone();
//
//     thread::spawn(move || {
//         while let Ok(evt) = touch_screen.recv() {
//             tx2.send(evt).ok();
//         }
//     });
//
//     let tx3 = tx.clone();
//     thread::spawn(move || {
//         while let Ok(evt) = usb_port.recv() {
//             tx3.send(Event::Device(evt)).ok();
//         }
//     });

pub fn parse_raw_events(paths: &[String], tx: &Sender<InputEvent>) -> Result<(), Error> {
    let mut files = Vec::new();
    let mut pfds = Vec::new();

    for path in paths.iter() {
        let file = File::open(path)
                        .with_context(|| format!("can't open input file {}", path))?;
        let fd = file.as_raw_fd();
        files.push(file);
        pfds.push(libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
    }

    loop {
        let ret = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, -1) };
        if ret < 0 {
            break;
        }
        for (pfd, mut file) in pfds.iter().zip(&files) {
            if pfd.revents & libc::POLLIN != 0 {
                let mut input_event = MaybeUninit::<InputEvent>::uninit();
                unsafe {
                    let event_slice = slice::from_raw_parts_mut(input_event.as_mut_ptr() as *mut u8,
                                                                mem::size_of::<InputEvent>());
                    if file.read_exact(event_slice).is_err() {
                        break;
                    }
                    tx.send(input_event.assume_init()).ok();
                }
            }
        }
    }

    Ok(())
}



fn parse_usb_events(tx: &Sender<DeviceEvent>) {
    let path = CString::new("/tmp/nickel-hardware-status").unwrap();
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_NONBLOCK | libc::O_RDWR) };

    if fd < 0 {
        return;
    }

    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };

    const BUF_LEN: usize = 256;

    loop {
        let ret = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, -1) };

        if ret < 0 {
            break;
        }

        let buf = CString::new(vec![1; BUF_LEN]).unwrap();
        let c_buf = buf.into_raw();

        if pfd.revents & libc::POLLIN != 0 {
            let n = unsafe { libc::read(fd, c_buf as *mut libc::c_void, BUF_LEN as libc::size_t) };
            let buf = unsafe { CString::from_raw(c_buf) };
            if n > 0 {
                if let Ok(s) = buf.to_str() {
                    for msg in s[..n as usize].lines() {
                        if msg == "usb plug add" {
                            tx.send(DeviceEvent::Plug(PowerSource::Host)).ok();
                        } else if msg == "usb plug remove" {
                            tx.send(DeviceEvent::Unplug(PowerSource::Host)).ok();
                        } else if msg == "usb ac add" {
                            tx.send(DeviceEvent::Plug(PowerSource::Wall)).ok();
                        } else if msg == "usb ac remove" {
                            tx.send(DeviceEvent::Unplug(PowerSource::Wall)).ok();
                        } else if msg.starts_with("network bound") {
                            tx.send(DeviceEvent::NetUp).ok();
                        }
                    }
                }
            } else {
                break;
            }
        }
    }
}



struct TouchState {
    position: Point,
    pressure: i32,
}

impl Default for TouchState {
    fn default() -> Self {
        TouchState {
            position: Point::default(),
            pressure: 0,
        }
    }
}

pub fn parse_device_events(rx: &Receiver<InputEvent>, ty: &Sender<DeviceEvent>, rotation:i8) {
    println!("parse_dev_ev");
    let mut id = 0;
    let mut last_activity = -60;
    //let Display { mut dims, mut rotation } = display;
    let mut dims_x = CURRENT_DEVICE.dims.1;
    let mut dims_y = CURRENT_DEVICE.dims.0;
    let mut fingers: FxHashMap<i32, Point> = FxHashMap::default();
    let mut packets: FxHashMap<i32, TouchState> = FxHashMap::default();//empty hashmaps
    let proto = CURRENT_DEVICE.proto;
    //store each event in hasmap? with key and value? id is mutable,
    //kind code value.
    //touchstate struct stores point and pressure
    //point stores 2 co ords

    let mut tc = match proto {
        TouchProto::Single => SINGLE_TOUCH_CODES,
        TouchProto::MultiA => MULTI_TOUCH_CODES_A,
        TouchProto::MultiB => MULTI_TOUCH_CODES_B,
        TouchProto::MultiC => MULTI_TOUCH_CODES_B,//turn touch code into struct w hex values for x,y, area/pressure
    };
    //pub enum TouchProto {
    //     Single,
    //     MultiA,
    //     MultiB, // Pressure won't indicate a finger release.
    //     MultiC,
    //pub const SINGLE_TOUCH_CODES: TouchCodes = TouchCodes {
    //     pressure: ABS_PRESSURE,
    //     x: ABS_X,
    //     y: ABS_Y,
    // these are the linux codes for the events that tell you what the value is for
    // pub const MULTI_TOUCH_CODES_A: TouchCodes = TouchCodes {
    //     pressure: ABS_MT_TOUCH_MAJOR,
    //     x: ABS_MT_POSITION_X,
    //     y: ABS_MT_POSITION_Y,
    //major stands for major axis of the contact area
    // pub const MULTI_TOUCH_CODES_B: TouchCodes = TouchCodes {
    //     pressure: ABS_MT_PRESSURE,
    //     .. MULTI_TOUCH_CODES_A
    // }; fill rest of fields from A

    if proto == TouchProto::Single {
        packets.insert(id, TouchState::default());
    }
    //insert id finger=0, point 0,0,pressure,0
    //into packets/touchstate(pt(x,y)+pressure?), fingers/point,
    //if its multi touch then it remains empty

    let (mut mirror_x, mut mirror_y) = CURRENT_DEVICE.should_mirror_axes(rotation);
    println!("parse_dev_ev beforeswap,MX{:?} MY{:?} TCX{:?} TCY{:?} CDSWAR{:?}", mirror_x, mirror_y,tc.x,tc.y,CURRENT_DEVICE.should_swap_axes(rotation));
    if CURRENT_DEVICE.should_swap_axes(rotation) {
        //why true when upright orientation seems mistake, unless plato relies on swapped axes?
        //again never called in this implementation so we dk. in
        //plato context struct calls framebuffer rotation function to get a value...
        //how odd, Default for Nia is to swap axes? swap and mirror is not same,
        //only tc EVENT RAW EVENT CODE switched...
        mem::swap(&mut tc.x, &mut tc.y);
        dims_x = CURRENT_DEVICE.dims.0;
        dims_y = CURRENT_DEVICE.dims.1;
        //mem::swap(&mut CURRENT_DEVICE.dims.0, &mut CURRENT_DEVICE.dims.1);

        // } else if evt.code == KEY_ROTATE_DISPLAY {
        //                 let next_rotation = evt.value as i8;
        //                 if next_rotation != rotation {
        //                     let delta = (rotation - next_rotation).abs();
        //                     if delta % 2 == 1 {
        //                         mem::swap(&mut tc.x, &mut tc.y);
        //                         mem::swap(&mut CURRENT_DEVICE.dims.0, &mut CURRENT_DEVICE.dims.1);
        //                     }
        //                     rotation = next_rotation;
        //                     let should_mirror = CURRENT_DEVICE.should_mirror_axes(rotation);
        //                     mirror_x = should_mirror.0;
        //                     mirror_y = should_mirror.1;
        //                 }
    }
    println!("parse_dev_ev afterswap,MX{:?} MY{:?} TCX{:?} TCY{:?} CDSWAR{:?}", mirror_x, mirror_y,tc.x,tc.y,CURRENT_DEVICE.should_swap_axes(rotation));
    //let mut button_scheme = button_scheme;

    while let Ok(evt) = rx.recv() {
        //receive raw event from the channel from raw_event function,
        //loop executes only when channel has received something, 1 loop per receive
        //bind evt to the inner value from ok, and loop while it is ok?
        // when channel ends returns error... when returns nothing, block thread
        if evt.kind == EV_ABS { // 0,1,3 EV_ABS=3, event type, code, value
            if evt.code == ABS_MT_TRACKING_ID { //this is for fingers? is 57 event code
                if evt.value >= 0 { //max is 2
                    id = evt.value;//make id the event value
                    packets.insert(id, TouchState::default());
                    //finger 0 will come first, finger 1 and 2 later if exists
                    //packets is touchstate hashmap, initlaise 0/1/2,0,0 or
                    //raw events, will always send ABS_MT_TRACKING_ID if multi touch,
                    // otherwise will only send x,y or pressure
                    //so the earlier initialisation is replaced by this one if mt,
                    // at the end of each frame will be cleared to empty
                    //each frame contains multiple fingers and co ords and pressures potentially
                    // if proto == TouchProto::Single {
                    //     packets.insert(id, TouchState::default());
                    // }
                }
                //why fingers and packets, points and touch states? touch states contain points and pressure...
                //so for each finger id we have a co ordinate and for each finger id in packets we have a touch state with
                //point and pressure, why bother with two hash maps? I see chatgpt says finger persists across frames while packets
                //is reset at the end of each frame, thus fingers stores last known position
                //fingers will remove data if the pressure at that point becomes 0, which will be sent by the touchinput event
                //how often does a new event frame occur?
                //so we loop through each event, one finger has x,y,p, and each time each finger gets put into the hashmaps, but
                //here so far is only packets only at the end of the frame but fingers get entries
            } else if evt.code == tc.x { //touch code x component, depends what type of touch code,
                //if raw event is the x component
                if let Some(state) = packets.get_mut(&id) {
                    //get value from key, define as state
                    //Some or None, returns? packets gets touch states, which defaults to 0,0,0 on startup
                    //remember each event contains only x,y,pressure or syn to end the block
                    //get the finger id, which default is zero, but if earlier event says finger id
                    //which is now 0,1 or 2 , and bind to variable state
                    //but get_mut gives mutable reference to reference of id? modify it modify original data too
                    state.position.x = if mirror_x { //if should swap x and y? boolean true or false
                        // some unwraps option, set position X in point to event value or mirrored event value
                        dims_x as i32 - 1 - evt.value //why minus 1 and value?
                    } else { //if false and no want mirror
                        evt.value //set the state.position.x to evt.value?
                        //if we do swap tc x and y, x events get passed to y arm, y events get passed to x arm, but dims does not get changed?
                    };
                    println!("parse_dev_ev x, POSX{:?} MIRRX{:?} DIMSX{:?} EVT{:?}", state.position.x, mirror_x, dims_x, evt.value);

                }
            } else if evt.code == tc.y { //touch code y component, depends what type of touch code, it will depend on
                //what code is for what device
                //if we mirror, swap tc.x and tc.y so here is tc.x instead, well use y values for x?
                //but packets state still y! this is about mirror not swap?
                if let Some(state) = packets.get_mut(&id) {
                    //if packets value for id is some, bind to state. new scope so must redo here
                    state.position.y = if mirror_y {
                        dims_y as i32 - 1 - evt.value
                    } else {
                        evt.value
                    };
                    println!("parse_dev_ev y, POSY{:?} MIRY{:?} DIMSY{:?} EVT{:?}", state.position.y, mirror_y, dims_y, evt.value);
                }//again just checks if should swap axes, otherwise just set to the value
            } else if evt.code == tc.pressure { //if not x or y but pressure
                if let Some(state) = packets.get_mut(&id) {
                    //bind value from key id, finger 0,1 or 2, to state, which is returned from a
                    //some from .get_mut
                    state.pressure = evt.value; //set pressure value
                    if proto == TouchProto::Single && CURRENT_DEVICE.mark() == 3 && state.pressure == 0 {
                        state.position.x = dims_x as i32 - 1 - state.position.x;
                        //why minus 1? make sure not out of bounds?
                        mem::swap(&mut state.position.x, &mut state.position.y);
                        //swap axes or swap old data for new?
                    }//if we can only track 1 finger and device is TouchAB, and pressure in the raw event is 0,
                    //mirror the x position? and then swap x and y  ?
                }
            }
            //by here we may have 3 fingers each with point and pressure in packets, but fingers nothing yet
        } else if evt.kind == EV_SYN && evt.code == SYN_REPORT {
            // The absolute value accounts for the wrapping around that might occur,
            // since `tv_sec` can't grow forever.
            //End of raw event block
            if (evt.time.tv_sec - last_activity).abs() >= 60 {
                // let mut last_activity = -60; t-60 >=60? so t needs to be 120?
                last_activity = evt.time.tv_sec; //set last activity here to current time... so next time 1 minute passes
                ty.send(DeviceEvent::UserActivity).ok();
            }//send enum? to device event loop function?

            if proto == TouchProto::MultiB { //why only for multi b? no multi A??
                //bc only B has pressure while A has contact area instead?
                //c is same as b... so for all non single, cleared at end
                //but the below is clearing all the fingers that dont have packets associated? or if channel has closed,
                //but why send fingerup here? because if finger does not contain the id, send finger up?
                //if 0,0 =>delete 0,1 keep, 1,0 keep, 1,1 keep
                //why would sending channel become closed though? the receiver ?
                //event is ev syn
                fingers.retain(|other_id, other_position| {
                    //at this point packets has been initialised with non zero values
                    //but fingers has not yet been
                    //i see retain iterates over the hashmap, giving the pairs as closure input
                    //retain by definition uses a closure so the closure is part of retain,
                    // values come from it
                    //this closure is used as an input parameter for the function,
                    // so values come from the call itself
                    //retains only the values that match the predicate in which case is the closure?
                    //retain expects closure that takes inputs and returns a bool
                    //for fingers, check if packets contains same key, or send error in which case return true?
                    //we could be cycling the fingers from theprevious frame?
                    // check if theyre still present, bc fingers not
                    //yet updated only packets have been
                    packets.contains_key(&other_id) ||
                        //iterate through fingers which is currently empty and check
                        // if packets has the same finger
                        //which is either 0,1 or 2 which has the x,y and pressure values
                        //||or operator only executes second if first if false,
                        // thus whether send message of finger up
                        //only if fingers do not contain key and.. if we cant send the message,
                        // return true, otherwise, return false
                        //so retain does not keep this id, so message is sent of fingerup and we
                        // get rid of the key because? finger no longer present. packets is current frame, fingers is last frame
                        //we send the message if not present, and if message successfully sent, send false, meaning retain
                        //will not keep them

                        //first time around, fingers will be empty thus we will send fingerup?
                        // but fingers is empty how can we send anything?
                        // ah okay retain only executes if fingers is non empty
                    ty.send(DeviceEvent::Finger {
                        //sending channel? but the loop itself is based on rx receive channel?
                        //let event
                        //while let Ok(evt) = rx.recv() so for every ok event it continues the loop?
                        // and if false, no rx receiving from raw events,
                        // ty is sending to the channel created in this function/deviceevents function
                        id: *other_id, //other_id is 0? so is other_position right now?
                        time: seconds(evt.time),
                        status: FingerStatus::Up,
                        position: *other_position,
                        //retain requires ownership, * dereferences and gets actual value
                    }).is_err()
                    //if ty.send fails, return true, so if packets has the key finger id 0 or the channel is closed
                    //return true to retain, so only keep finger 0 in packets right now
                    //but if send succeeds, return false. if packets has finger id 1 or 2 return false
                    //initialising finger enum struct?
                    //pub enum DeviceEvent {
                    //     Finger {
                    //         id: i32,
                    //         time: f64,
                    //         status: FingerStatus,
                    //         position: Point,
                    //     },
                }); //end of retain function, end of closure body
            }

            for (&id, state) in &packets {
                // let mut fingers: FxHashMap<i32, Point> = FxHashMap::default();
                //  fingers not yet initialised
                // let mut packets: FxHashMap<i32, TouchState> = FxHashMap::default();
                // packets has been initialised but in
                // 1st pass will only contain one key value pair
                // pub struct Point {
                //     pub x: i32,
                //     pub y: i32,
                // struct TouchState {
                //     position: Point,
                //     pressure: i32,
                if let Some(&pos) = fingers.get(&id) {
                    //bind inner value of fingers.get(&id) to pos, returns a point struct?
                    //&pos says dereference the right side
                    //if fingers has key id 0, bind the pos to the value of the point
                    if state.pressure > 0 { //default initalisation is zero
                        //state is from packets, which means the values are non zero by now
                        if state.position != pos {
                            //if the state position in packet hashmap has changed to the last position in fingers hashmap
                            //if first time, fingers is empty and this block doesnt execute
                            //state.position have already processed and now we are at end of event frame
                            ty.send(DeviceEvent::Finger {
                                id,
                                time: seconds(evt.time),
                                status: FingerStatus::Motion,
                                position: state.position,
                            }).unwrap();
                            fingers.insert(id, state.position);
                            //same id, but insert the state.position? from packets, overwriting the old value

                        }
                    } else { //if state pressure is 0, which might mean we havent
                        // looped through any events yet? no if no events yet this doesnt even execute, finger
                        //has been lifted, which sends a raw event saying that the
                        // pressure at that point has become 0 from 1024?
                        //will a raw event be send to say pressure has become zero? apparently yes
                        ty.send(DeviceEvent::Finger {
                            id,
                            time: seconds(evt.time),
                            status: FingerStatus::Up,
                            position: state.position,
                        }).unwrap();
                        fingers.remove(&id);//remove the data bc finger no longer touching screen
                    }
                } else if state.pressure > 0 {
                    //we looping through packets
                    //if let Some(&pos) = fingers.get(&id) {
                    // if fingers does not contain that id,
                    //the first time, fingers is empty, thus this block is executed
                    ty.send(DeviceEvent::Finger {
                        id,
                        time: seconds(evt.time),
                        status: FingerStatus::Down,
                        position: state.position,
                    }).unwrap();
                    fingers.insert(id, state.position);
                }//add event data to finger, now is no longer empty
            }

            if proto != TouchProto::Single {
                packets.clear();
            }//remove all packets if not single finger device? if it is a single finger device...
            // then there wont be more than 1 finger at a time
            //this only triggers at end of each block

            //need to go through example raw events to see what happens eh imagine the actual flow
        }// else if evt.kind == EV_KEY {
        //     //buttons
        //     if SLEEP_COVER.contains(&evt.code) {
        //         if evt.value == VAL_PRESS {
        //             ty.send(DeviceEvent::CoverOn).ok();
        //         } else if evt.value == VAL_RELEASE {
        //             ty.send(DeviceEvent::CoverOff).ok();
        //         } else if evt.value == VAL_REPEAT {
        //             ty.send(DeviceEvent::CoverOn).ok();
        //         }
        //     } else if evt.code == KEY_BUTTON_SCHEME {
        //         if evt.value == VAL_PRESS {
        //             button_scheme = ButtonScheme::Inverted;
        //         } else {
        //             button_scheme = ButtonScheme::Natural;
        //         }
        //     } else if evt.code == KEY_ROTATE_DISPLAY {
        //         let next_rotation = evt.value as i8;
        //         if next_rotation != rotation {
        //             let delta = (rotation - next_rotation).abs();
        //             if delta % 2 == 1 {
        //                 mem::swap(&mut tc.x, &mut tc.y);
        //                 mem::swap(&mut CURRENT_DEVICE.dims.0, &mut CURRENT_DEVICE.dims.1);
        //             }
        //             rotation = next_rotation;
        //             let should_mirror = CURRENT_DEVICE.should_mirror_axes(rotation);
        //             mirror_x = should_mirror.0;
        //             mirror_y = should_mirror.1;
        //         }
        //     } else if evt.code != BTN_TOUCH {
        //         if let Some(button_status) = ButtonStatus::try_from_raw(evt.value) {
        //             ty.send(DeviceEvent::Button {
        //                 time: seconds(evt.time),
        //                 code: ButtonCode::from_raw(evt.code, rotation, button_scheme),
        //                 status: button_status,
        //             }).unwrap();
        //         }
        //     }
        // } else if evt.kind == EV_MSC && evt.code == MSC_RAW {
        //     if evt.value >= MSC_RAW_GSENSOR_PORTRAIT_DOWN && evt.value <= MSC_RAW_GSENSOR_LANDSCAPE_LEFT {
        //         let next_rotation = GYROSCOPE_ROTATIONS.iter().position(|&v| v == evt.value)
        //                                                .map(|i| CURRENT_DEVICE.transformed_gyroscope_rotation(i as i8));
        //         if let Some(next_rotation) = next_rotation {
        //             ty.send(DeviceEvent::RotateScreen(next_rotation)).ok();
        //         }
        //     }
        // }
    }
}
