# eInk VNC

From noobyme:

I have fixed ZRLE DroidVNC issues using claude AI. 

I have changed rotation default to use plato's function to see which value to use, so that the correct default upright orientation is used. 

I have added a contingency device detection using /mnt/onboard/.kobo/version instead of using environment variables because starting the tool over ssh does not pass those values. 

I have also added touchscreen functionality from plato input.rs

I have copied over the ClaraColor and LibraColor device.rs from plato's latest version, so that those devices may be correctly detected if used, Im unsure if the actual program will work however, but I do know that its possible for an incorrectly detected device to still work, and in the future so may devices not yet released as the after detecting the device, only 2 paths emerge, KoboFrameBuffer1 or KoboFrameBuffer2. Only Mark8 devices do KFB1, everything else does KFB2.

The original commit did not have an issue with zrle droidvnc unless it was a debug compile, in which case it would crash after briefly appearing to work due to it being too slow, apparently. Idk for sure I asked claude to help me. The latest commit however does have an issue, zrle droidvnc doesnt work at all. The compiled file provided by anchovy is the oldest commit one, but you cannot rotate the screen with it

Rotate to landscape display using flag --rotate 2. Your device landscape number might be different
Use resolution smaller than or exactly equal to your display. eg common resolution of 1024x768 will fail to work correctly on Kobo Nia because 1024x758 is the maximum. Custom resolution of 1024x758 works!
To stop all other programs use this command before launching eink-vnc, thanks koreader startup script.

killall -q -TERM nickel hindenburg sickel fickel strickel fontickel adobehost foxitpdf iink dhcpcd-dbus dhcpcd bluealsa bluetoothd fmon nanoclock.lua

Compilation instructions

To compile on wsl ubuntu noble 24.04, x86_64 CPU
1 Go to linux user home directory
2 Clone repository
3 Download linaro cross toolchain file (the toolchain itself will do no need for sys root file)
We want gcc-linaro-4.9.4-2017.01-x86_64_arm-linux-gnueabihf.tar.xz
4 Extract toolchain
5

cd /home/noobyme
git clone https://github.com/everydayanchovies/eink-vnc
wget https://releases.linaro.org/components/toolchain/binaries/4.9-2017.01/arm-linux-gnueabihf/gcc-linaro-4.9.4-2017.01-x86_64_arm-linux-gnueabihf.tar.xz
tar -xf gcc-linaro-4.9.4-2017.01-x86_64_arm-linux-gnueabihf.tar.xz
cd /eink-vnc/client/
mkdir .cargo
nano .cargo/config.toml

[target.armv7-unknown-linux-gnueabihf]
linker = "/home/noobyme/gcc-linaro-4.9.4-2017.01-x86_64_arm-linux-gnueabihf/bin/arm-linux-gnueabihf-gcc"

sudo apt-get install rustup
rustup target add armv7-unknown-linux-gnueabihf
cargo build --target armv7-unknown-linux-gnueabihf

Done

The first time round I was got an error for these libraries, but seems unneeded,  looks like it was because I had this in the config.
rustflags = [
"-C", "link-arg=--sysroot=/home/noobyme/gcc-linaro-4.9.4-2017.01-x86_64_arm-linux-gnueabihf/arm-linux-gnueabihf/libc",
]
if you run into that, extra steps to fix above error:

sudo dpkg --add-architecture armhf
sudo add-apt-repository multiverse
sudo add-apt-repository universe
sudo nano /etc/apt/sources.list.d/ubuntu.sources

Types: deb
URIs: http://archive.ubuntu.com/ubuntu/
Suites: noble noble-updates noble-backports
Components: main universe restricted multiverse
Architectures: amd64
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg

Types: deb
URIs: http://security.ubuntu.com/ubuntu/
Suites: noble-security
Components: main universe restricted multiverse
Architectures: amd64
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg

Types: deb
URIs: http://ports.ubuntu.com/ubuntu-ports/
Suites: noble noble-updates noble-backports noble-security
Components: main restricted universe multiverse
Architectures: armhf
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg

sudo apt update
sudo apt-get install libz-dev:armhf libbz2-dev:armhf libjpeg-dev:armhf libpng16-dev:armhf libgumbo-dev:armhf libopenjp2-dev:armhf libjbig2dec-dev:armhf

cd /usr/lib/arm-linux-gnueabihf
cp libz.* libbz2.* libjpeg.* libpng16.* libgumbo.* libopenjp2.* libjbig2dec.* /home/noobyme/gcc-linaro-4.9.4-2017.01-x86_64_arm-linux-gnueabihf/arm-linux-gnueabihf/libc/lib

Note:Copy and pasted commands from AI can fail because formatting differences
Use AI to help, thats how I successfully compiled it.
https://www.mobileread.com/forums/showthread.php?t=348481&page=2 Thanks elinkser
Failed to fill whole buffer? Tightvnc server side blocked ip

Eh, i didnt realise theres an easier way, using docker, from chatgpt
Ubuntu 24.04, non WSL

sudo apt update
sudo apt upgrade -y
sudo apt install -y apt-transport-https ca-certificates curl software-properties-common lsb-release gnupg
curl -fsSL https://download.docker.com/linux/ubuntu/gpg | sudo gpg --dearmor -o /usr/share/keyrings/docker-archive-keyring.gpg
echo
"deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/docker-archive-keyring.gpg]
https://download.docker.com/linux/ubuntu $(lsb_release -cs) stable" |
sudo tee /etc/apt/sources.list.d/docker.list > /dev/null
sudo apt update
sudo apt install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
sudo systemctl start docker
sudo systemctl enable docker
sudo usermod -aG docker $USER (then log out and log in)
docker --version
docker run hello-world
docker pull ewpratten/kobo-cross-armhf:latest
sudo apt-get install cargo
sudo apt-get install rustup
rustup default stable
cargo install cross
echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
cd /eink-vnc/client
cross build --target arm-unknown-linux-musleabihf --release

From original readme:

A lightweight CLI (command line interface) tool to view a remote screen over VNC, designed to work on eInk screens.
For now, you can only view, so you'll have to connect a keyboard to the serving computer, or find some other way to interact with it.

This tool has been confirmed to work on several Kobo devices, such as the Kobo Libra 2 and Elipsa2E.
It was optimized for text based workflows (document reading and writing), doing that it achieves a framerate of 30 fps.

As VNC server we tested successfuly with TightVNC, x11vnc and TigerVNC.


## Warning

The screen can refresh up to 30 times per second, this will degrade the eInk display rapidly.
Do not use with fast changing content like videos.

Furthermore, this tool was only tested on Kobo Libra 2 and Kobo Elipsa 2E.
**It is possible that it will damage yours.**
*I cannot be held responsible, use this tool at your own risk.*

[einkvnc_demo_kobo_rotated.webm](https://user-images.githubusercontent.com/4356678/184497681-683af36b-e226-47fc-8993-34a5b356edba.webm)

## Usage

You can use this tool by connecting to the eInk device through SSH, or using menu launchers on the device itself.

To connect to a VNC server:

``` shell
./einkvnc [IP_ADDRESS] [PORT] [OPTIONS]
```

For example:

``` shell
./einkvnc 192.168.2.1 5902 --password abcdefg123 --contrast 2 
```

For faster framerates, use USB networking (see https://www.mobileread.com/forums/showthread.php?t=254214).

## Derivatives

The code responsible for rendering to the eInk display is written by baskerville and taken from https://github.com/baskerville/plato.
The code responsible for communicating using the VNC protocol is written by whitequark and taken from https://github.com/whitequark/rust-vnc.
