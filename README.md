# StarPlug

APM-based vibration for Starcraft 1 and Brood War.

## Requirements

- [Intiface Central](https://intiface.com/central/): part of the [Buttplug.io](https://buttplug.io/) project.
- One or more [vibrators supported by Buttplug.io](https://iostindex.com/).
- [StarCraft](https://starcraft.com/), either the [free](https://battle.net/download/getInstallerForGame?version=LIVE&gameProgram=STARCRAFT) or Remastered versions.
- macOS.
- The [`lldb`](https://lldb.llvm.org/) debugger: installed with Xcode, from [Apple's developer downloads](https://developer.apple.com/downloads/) or by running `xcode-select --install` from the command line.

## Notes

StarPlug uses `lldb` to find the code that calculates APM and sets a script breakpoint on it so that the value can be sent to StarPlug itself.

StarPlug does not make any changes to your StarCraft files, and can be safely deleted if you get bored of it. There is no uninstall necessary.

StarPlug currently requires macOS, and is tested on an Intel Mac running macOS 13 aka Ventura, not yet on Apple Silicon. Windows is not supported yet.

StarPlug requires a version of StarCraft that's new enough to have an in-game APM display. So far, I've tested it with 1.23. I'd love to get it working with a pre-Remastered BWAPI-capable version like [1.16.1](https://www.cs.mun.ca/~dchurchill/starcraftaicomp/resources.shtml), or with [OpenBW](http://www.openbw.com/) ([GitHub](https://github.com/OpenBW/openbw)), but the route to extracting APM info will likely be different for those.

It definitely does not work with StarCraft II. That'll be fun to figure out.

## Instructions

- Install and open Intiface Central.
- Press the ▶️ button to start its server.
- Click the Devices tab.
- Click "Start scanning".
- Connect your vibrator and make sure it shows up in the device list.
  - Use the sliders to make sure it works.
- Click "Stop scanning".
- Open a terminal window and run `starplug --help`.
  - If you've checked out this repo instead of using a prebuilt StarPlug, `cargo run -- --help`.
- Run `starplug`.
  - Or `cargo run`. 
  - You can run with the defaults, or pass extra command-line arguments to change the APM range.
- Intiface Central should show that StarPlug is connected.
- Open the Battle.net launcher.
- Use it to start Starcraft.
  - ⚠️ Currently, you must start StarCraft only after starting StarPlug. This will be fixed in a later release.
- Go to "Options" from the main menu, click the "Game" tab, check the checkbox for "Display APM In Game", and then click the "Ok" button to save your options.
- Start a game.
- Your vibrator will begin vibrating when you reach the min APM, and you need to reach at least the max APM to get the highest vibration level.
  - ⚠️ Currently, you need to quit StarCraft to stop vibration. This will be fixed in a later release. You can always Ctrl-C StarPlug in the terminal to stop it quickly, but you will need to restart both StarPlug and StarCraft for StarPlug to work after that.
- Enjoy!
