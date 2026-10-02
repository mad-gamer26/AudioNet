# Accessibility

AudioNet must be fully usable by blind people. This is a requirement, not
polish: a feature that cannot be used with a screen reader is not finished.

## Commitments

* **Everything important is text.** Device state, stream state, errors
  and diagnostics are always written out in words. Nothing is conveyed only
  by color, graphs, meters, waveforms, position, hover text or animation.
* **Native controls.** Buttons, selects, checkboxes and forms from the
  platform, each with a visible label. No custom widgets where a native one
  exists.
* **Keyboard complete.** Every action works from the keyboard, in a logical
  order, with a visible focus indicator.
* **Calm announcements.** State changes (signed in, device online, stream
  connected, stream stopped with reason) are announced once through a
  polite live region; errors through an alert. Periodic measurements are
  never announced automatically.
* **Stable focus.** Updates change existing elements in place, so a
  screen reader's position and focus are not lost when a device goes online
  or a list refreshes. After significant actions focus moves somewhere
  sensible (the devices heading after sign-in, the sign-in heading after
  sign-out, the streams heading when the focused stream stops).
* **Command-line output is linear.** `audionet list` and every report use
  one "Label: value" fact per line, positions such as "Output device 2 of
  7", no tables, columns, color or cursor movement. Every report can also
  be produced as JSON.
* **Errors name the failing part,** for example "WASAPI audio playback
  failed while checking the endpoint state: the device was disconnected,
  disabled, or reconfigured."
* **Audio threads never starve the screen reader.** AudioNet uses MMCSS for
  its audio threads, never `REALTIME_PRIORITY_CLASS`.

## Several accounts (Windows, Mac and iPhone apps)

An app can be signed in to several AudioNet accounts at once; it is then a
separate device in each (the web client stays one account at a time).
Sharing is chosen per account (see *Online and sharing*).

* Windows: an "Accounts" list (one line per account: "mad-gamer26 on
  audionet.example.com, as "Studio PC": online, sharing") and "Sign out of
  the selected account" (it asks first). The sign-in fields stay available;
  the button then reads "Add account". With several accounts the device
  tree has one top-level item per account ("Devices in mad-gamer26 on
  audionet.example.com (3 devices)", open by default) holding its
  devices.
* Mac: an Accounts section in the window, each account with its own Sign
  Out button (named "Sign Out of mad-gamer26 on ...") and "Add Account…",
  which opens the sign-in form as a sheet (Escape cancels).
* iPhone: Settings, Accounts: each account with "Sign Out of ..." and
  "Add Account", which opens the sign-in form.
* With several accounts, the devices come in one section per account,
  headed "Devices in mad-gamer26 on audionet.example.com".

## Online and sharing (Windows, Mac and iPhone apps)

While an app runs it is online in every account it is signed in to; a
device is offline only when its AudioNet is not running. Whether it
*shares its audio* is chosen per account: sharing, other devices can
listen to it and it can send its own audio; not sharing, it still sees
the devices, listens to them and plays what they send it, but sends
nothing of its own. Each account's choice is kept; an account signed in
to later starts without sharing. Other devices (and the web client) show
it as "online, not sharing" rather than offline, and explain in words
that it cannot be listened to but can be sent to; starting or stopping
sharing is announced like going online or offline.

* Windows: "Start sharing" / "Stop sharing" acts on the account selected
  in Accounts ("... in the selected account" when there are several); the
  tray menu's Start sharing / Stop sharing acts on every account.
* Mac and iPhone: a switch per account, "Share This Mac's Audio" /
  "Share This iPhone's Audio" ("... in mad-gamer26 on ..." when there are
  several), named on the switch itself; the Mac's menu bar menu has Start
  Sharing / Stop Sharing for every account.
* Sending needs sharing: without it the Send button is unavailable and a
  line says why ("Turn on Share This Mac's Audio to send from this Mac.").
* Sign Out asks first, then removes the device from that account on the
  server (nothing is left behind), whether it shares or not.

## Stream volume and mute (every app)

Every stream an app started has its own Mute switch and Volume slider
(0 to 100 percent, in steps of 5), on this device only: for a stream it
listens to, what plays here; for one it sends, what is sent. Their spoken
names include the stream ("Volume for Listening to Speakers on Studio PC",
"Mute Listening to ..."); the slider's value is "50 percent" or "50
percent, muted". The slider is heard on a square curve (half-way is about
12 dB quieter), the same in every app. Changes apply at once and ramp
over 10 ms, so they never click.

* Windows: "Mute the selected stream" (a check box) and "Volume of the
  selected stream" (a standard trackbar: arrow keys 5, Page Up and Page
  Down 10) follow the stream selected in the list; the list line also
  says "volume 50 percent" and "muted" when changed.
* Mac and iPhone: a Mute switch and a Volume slider in each stream's row.
* Web: a Mute check box and a Volume range input in each stream's line
  (`aria-valuetext` gives the percentage).

## Creating an account

Where the server allows it:

* Web: the sign-in page has "New to AudioNet? Create an account" (a
  button). It opens a "Create an account" section; focus moves to its
  heading and a name already typed on the sign-in form is carried over.
  The username and password fields carry their rules as descriptions
  ("Letters, digits, dots, dashes and underscores, up to 64 characters";
  "At least 10 characters, and not containing your username"). Different
  passwords, a taken name or a refused name are reported as an alert, and
  focus moves to the field to fix, marked invalid. Creating the account
  signs in and announces "Account NAME created. You are signed in."; "Back
  to sign in" returns to the sign-in form.
  The "Email address" field is required, and its description says what
  it is for ("Used only to reset your password if you forget it").
* The apps and the command line only sign in: people create their account
  in the web client before installing AudioNet.

## Email address and forgotten passwords

* Web, signed in without an email address: a region above the devices,
  headed "Add an email address", says "An email address is highly
  recommended. It is used only to reset your password if you forget it.",
  with an "Add an email address" button; signing in also announces that
  the account has no address. With an address waiting for confirmation
  the region is headed "Confirm your email address", says where the link
  went, that it works for 7 days, and (when changing) that reset links
  still go to the confirmed address meanwhile; it has "Send the link
  again".
* The "Email address" part (a heading, after "Add a device") states the
  address in words ("Email address: NAME, confirmed."). Its button is a
  disclosure (`aria-expanded`) for a form with the address and the
  account password; focus goes to the address field, errors are alerts
  with focus on the field to fix, and saving announces the emailed link.
* Opening an emailed link announces the result ("Email address ...
  confirmed"); the link's token is removed from the address bar.
* "Forgot your password?" (a button on the sign-in page) opens "Reset your
  password" with focus on its heading; the answer is shown and announced.
  The reset link opens "Choose a new password" with focus on its heading;
  setting it signs in and announces it, with focus on "Your devices".
* Windows: a "Forgot password…" button (Alt+F) after "Sign in"; Mac and
  iPhone: a "Forgot Password?" button after "Sign In". Each opens the
  server's reset page in the browser, or, without a server address, says
  so and (Mac) moves focus to that field.

## NVDA add-on

* NVDA's own conventions: a Tools menu item (AudioNet...), an AudioNet
  category in NVDA's settings (accounts, announcement choices), and
  commands in Input Gestures under AudioNet.
* NVDA+Alt+A turns on AudioNet commands: W or O open the window, L listen
  again, S stop all, R report streams, M mute or unmute, up and down arrows
  volume, D report devices, H list the keys. They stay on until Escape,
  NVDA+Alt+A again, or opening the window (W or O); another key says it is
  not an AudioNet command and that Escape leaves.
* The AudioNet window uses standard controls, each labelled through NVDA's
  guiHelper: an Account choice (with its connection state in words), a
  Devices tree (a device, then "Sounds to listen to" and "Outputs to send
  to"; Enter listens or sends), Play on and Send from choices, a Streams
  list, Stop, a Volume slider (5 percent per arrow, 10 per page) and a Mute
  check box. Updates change items in place so NVDA's position is kept.
* Streams connecting and ending (other than by Stop) are announced; devices
  going online or offline can be. The status log is read-only text that
  starts at the newest event; with "Show measurements" on (AudioNet
  settings, off by default) each stream's measurements follow.
* Tested in a running NVDA by the maintainer: fully working (2026-10-02).

## Web client specifics

* Semantic landmarks (`header`, `main`, `footer`) and a skip link.
* Headings: "Sign in", "Your devices", "Active streams", "Add a device",
  and "Measurements" in the status log when shown.
* Each device is a native disclosure (`<details>`/`<summary>`): one line,
  "Studio PC: online, Windows, 11 sounds, 6 outputs", announced as
  collapsed or expanded; Enter or Space opens it to its listen and send
  controls. Refreshes keep what is open and the focus
  (`scripts/test/web_devices_a11y.py` checks this in Chrome's
  accessibility tree).
* "Add a device" gives this server's address and the terminal sign-in
  command as plain text, with a copy button.
* A "Status log" button opens a modal dialog (a native `<dialog>`) named
  "Status log": the Events (every announcement and problem so far, with
  the time), a "Show measurements" check box (off by default, kept in the
  browser) and, when checked, the Measurements (plain-text measurements
  for each stream, never announced); both are focusable text blocks, then
  Copy and Close.
  Focus moves to the events; Escape or Close closes it and focus returns
  to the button (`scripts/test/web_status_log.py` checks this in Chrome's
  accessibility tree).
* Respects `prefers-color-scheme` and `prefers-reduced-motion`. Minimum
  control height 44 CSS pixels.

## Windows desktop app: remote

The app signs in with the account name and password (the password field
is cleared as soon as it is used). Once started, a second column has
**Your devices**, a standard tree view (the Windows outline control): each
device is one collapsed item ("Name, online", "Name, online, not
sharing" or "Name, offline"). Right
Arrow (or the expand button) opens it to **Sounds to listen to (N)** and
**Outputs to send to (N)**, each with its items; screen readers announce
names, levels, positions and expanded states from Windows itself. Enter on
a sound listens to it on **Play it on**; Enter on an output sends **Send
from this computer** to it (the buttons **Listen to the chosen sound** and
**Send to the chosen output** do the same). Then **Streams this computer
started** with **Stop the selected stream**. Changes are made in place, so
what is expanded, the selection and the screen reader's position are kept.
The controls stay enabled (so they stay in the Tab order); if a choice
cannot be used, Listen and Send say why in words ("… is offline", "… is
not sharing its audio", "This computer is not sharing its audio in this
account", "Choose a sound first: expand …"). Devices going online or
offline, starting or stopping sharing, and streams starting, connecting
and stopping, are announced once.

## Windows desktop app: Settings

The options are in their own **AudioNet Settings** window, opened with the
main window's **Settings…** button (Alt+G) or the tray menu's
**Settings…**: start at Windows sign-in, start sharing when AudioNet
opens, keep running in the system tray, start minimized to the tray, keep
up to date automatically, and **Check for updates now**. It also shows the
latest update result and the version. Focus starts on the first option;
each option is a standard checkbox with an access key and applies at
once; Escape or **Close** closes the window and focus returns to where it
was (normally the Settings button). Announcements come from Settings while
it is in front.

## Windows desktop app: system tray

Closing the window keeps AudioNet running in the system tray (an option in
Settings turns this off); the first time, a Windows notification says where it went
and how to exit, and screen readers read that notification. The tray icon
is reached with Windows+B and the arrow keys. Its name is its tooltip,
which always states what AudioNet is doing ("AudioNet: sharing, signed in as
…", "AudioNet: not sharing, …", "AudioNet: sharing in 1 of 2 accounts",
"AudioNet: not signed in"). Enter opens the
window; the Applications key or Shift+F10 opens a standard menu (Open
AudioNet, Start or Stop sharing, Settings…, Exit). If AudioNet stops with an error
while the window is hidden, a notification says so. Starting AudioNet
again while it runs brings back the existing window instead of a second
copy. Each account's sharing is kept across restarts (0.7's "Start
sharing automatically when AudioNet opens" was replaced by it).

A hidden window never holds the keyboard focus, because a screen reader
would follow the focus and read the window as if it had opened. Starting
in the tray leaves the focus where it was, and after a tray menu choice
that does not open the window (Start or Stop sharing, or Escape), the
focus returns to where it was, normally the tray icon.

The status log opens from the **Status log…** button in a window of its
own ("AudioNet Status Log"): a read-only multi-line edit named "Status
log" that screen readers move through line by line, with Copy and Close
(Escape also closes it). Focus starts in the log at the newest line and
returns to the button when the window closes. Important lines are spoken
as they happen whether or not the log is open.

Updates install themselves (see [releasing.md](releasing.md)); every step
is written to the status log, and after an update the new copy says
"AudioNet was updated from version X to Y", aloud if its window is open
or as a Windows notification if it is in the tray. **Check for updates
now** reports "AudioNet is up to date" or what it is doing. In builds
without an update source, the update option and button in Settings are
shown as unavailable.

## macOS app

Standard controls in a grouped form, so VoiceOver reads every field,
menu and button with its name, role and state:

- Sign-in fields are labeled (server address, account name, password,
  name for this Mac). Submitting with a field empty says which information
  is missing and moves focus to that field.
- Each device is a native disclosure (DisclosureGroup): one line, "name:
  online", read by VoiceOver as a collapsed or expanded disclosure
  triangle (VO-Space toggles it; a click anywhere on the line does too).
  Inside are the device's listen and send choices, remembered per device.
  They are AppKit pop-up menus labeled with what they choose (VO-Space
  opens one), because SwiftUI pop-up pickers report no press action to the
  macOS 27 audit. Listen and Send name their device ("Listen to Studio
  PC").
- Each stream is a line of text such as "Listening to Speakers on Studio
  PC, playing on MacBook Air Speakers: connected",
  and, with "Show Measurements" on in Settings (off by default), its
  measurements in a second line that VoiceOver reads on demand and never
  announces. Each Stop button names its stream.
- Changes that matter (online, a stream connected or ended, a warning such
  as a silent microphone, errors) are announced with VoiceOver
  announcements and also added to the status log. The **Status Log…**
  button opens it in a dialog (a sheet): a read-only text area VoiceOver
  reads line by line, Copy, and Close (Escape also closes it).
- The menu bar item (VO-M, then M, reaches the menu bar extras) is named
  "AudioNet, sharing" or "AudioNet, not sharing", and its menu says
  whether this Mac shares its audio and has Open AudioNet, Start Sharing
  or Stop Sharing (every account), Settings…, and Quit.
- With "Keep AudioNet running in the menu bar" on, closing the window also
  removes the Dock icon (AudioNet is then only in the menu bar, not in
  the Dock or Command-Tab) and VoiceOver says "AudioNet is still running
  in the menu bar." "Start AudioNet in the menu bar" starts it that way.
  Opening AudioNet again (Spotlight, Finder) or Open AudioNet brings back
  the window and the Dock icon. UI tests check the Dock icon through macOS
  itself; XCTest cannot open a menu bar menu of an app without a Dock icon
  (or any menu while the Mac's screen is off), so choosing items in that
  menu is part of the manual VoiceOver test, not the automated one.

`apps/macos/AudioNetUITests` (run by `scripts/test/mac_ui_tests.py`) runs
Xcode's accessibility audit on the sign-in window and on the signed-in
window with a stream running, checks that an empty sign-in says what is
missing and moves focus there, and goes through opening online and not
sharing (a named switch per account), choosing a device and sounds from
the keyboard-reachable menus, listening while not sharing, sending
unavailable until sharing, sharing, stopping, and signing out (asked
first). One audit finding is ignored: a "parent/child mismatch"
with no element, which a window holding a single plain SwiftUI text field
also gets (the focused field's editor, macOS 27).

## iPhone app

A native SwiftUI app built from standard controls: a Form with prominent
section headings, text fields that wrap long entries, each device a
`DisclosureGroup` ("Studio PC: online", collapsed or expanded), menu
pickers for the sound and output, buttons whose spoken names say what
they act on ("Listen to Studio PC", "Stop Listening to ..."), a
Microphone row that says which microphone records ("iPhone Microphone,
Front, stereo") and opens a list like UniMic's (a section per input, a
button per source, the chosen one marked selected; the list redraws only
when the microphones change, so VoiceOver keeps its place), and a Status
Log row that opens the log on its own
screen, one row per line, with Copy at the top right. Everything announced
is also in the log. A sign-in problem is also shown on the form itself.
Settings and Status Log are rows, not navigation-bar buttons, because bar
buttons do not grow with Dynamic Type (Copy is the one bar button: it
has a text equivalent in the log's rows). The accent color is darker (light mode) or
lighter (dark mode) than the system blue, which falls short of 4.5:1 on
grouped backgrounds.

The UI tests (`apps/ios/AudioNetUITests`, run by
`scripts/test/ios_ui_tests.py`) accept these audit findings, each only
with a measurement or a stated position:

* Contrast: the audit reported plain black-on-white text as failing. The
  test measures the element's rendered pixels and accepts the finding
  only at 4.5:1 or more; or the element is partly under the translucent
  navigation bar (scrolled there), where the audit measures through the
  bar's blur.
* Dynamic Type and clipped-text findings with no element: the signed-in
  screen gets them
  on the simulator since 2026-09-27, also with the app as it was before
  that day's changes; they name nothing to check or fix, so they are
  logged, not failed on.
* Dynamic Type "partially unsupported" on plain text (never on controls):
  the audit reports whichever text sits lowest on the screen (status log
  rows, a stream's measurements, the last section heading, short or long),
  because at its largest sizes that text would run off a screen it does
  not scroll. The app sets no fixed font sizes, and
  `testTextGrowsWithDynamicType` measures a section heading and a log row
  at more than 1.5 times their height at an accessibility size.

## Verification status

Be precise about what has been checked:

| Check | Status |
| --- | --- |
| Web client semantic structure, labels, focus moves, announcements (automated in real Chrome via Selenium) | Done |
| CLI output format (golden-text unit tests) | Done |
| NVDA (Windows) and VoiceOver (macOS, iOS) with the web client | Done: tested by the maintainer, fully working (2026-10-02) |
| NVDA / JAWS with `audionet` command-line output | **Not yet tested** |
| Windows desktop app names, roles and sign-in/start flow (automated through UI Automation, `scripts/test/desktop_uia.py`) | Done |
| Windows desktop app Settings window (named, focus on the first option, Escape closes it and focus returns, opens from the tray menu while hidden) and system tray: named checkboxes, icon present, close to tray, Enter on the icon, named tray menu items, clean exit, start in tray, focus never left in the hidden window (automated, `scripts/test/desktop_tray.py`); sharing starting by itself after a restart when chosen (`desktop_uia.py`) | Done |
| Windows desktop app remote: sign in, choose a device and sounds with the keyboard, Listen and Send with the audio checked, stop (automated through UI Automation, `scripts/test/desktop_remote.py`) | Done |
| NVDA with the Windows desktop app, including the remote, the tray icon and its notifications | Done: tested by the maintainer, fully working (2026-10-02) |
| The NVDA add-on in a running NVDA (window, settings panel, command layer, announcements) | Done: tested by the maintainer, fully working (2026-10-02) |
| iPhone app: Xcode accessibility audit of the sign-in screen and of the signed-in screen with a stream running; empty sign-in; expanding a device, listening, collapsing, stopping (`scripts/test/ios_ui_tests.py`, iOS simulator) | Done |
| VoiceOver on iOS with the app | Done: tested by the maintainer, fully working (2026-10-02) |
| macOS app: Xcode accessibility audit of the sign-in and signed-in windows, sign-in, listen and stop flows (`apps/macos/AudioNetUITests`, `scripts/test/mac_ui_tests.py`) | Done |
| VoiceOver on macOS with the app | Done: tested by the maintainer, fully working (2026-10-02) |

Automated checks prove structure, not usability. Each row still marked
**Not yet tested** needs a real screen-reader session, and so does any new
user-interface change, on every platform.

## Manual test script (web client, about 10 minutes)

With a screen reader running (NVDA, JAWS or VoiceOver):

1. Load the site. Navigate by headings: you should hear "AudioNet",
   "Sign in". Tab through: Username, Password, Sign in.
2. Sign in with a wrong password: an alert with the reason is read, focus
   returns to the password field.
3. Sign in correctly: "Signed in as NAME" is announced; focus is on the
   "Your devices" heading.
4. Under "Add a device", the server address and the sign-in command are
   read; the copy button works.
5. Sign a device in and start it: "NAME is now online" is announced once.
6. In the device's "Sound to listen to" select, arrow through the sources;
   press Listen: "Starting …" then "… connected" are announced.
7. In the status log, check "Show measurements" and move to "Measurements":
   the text reads line by line and is not announced on its own while you
   do other things.
8. Press the stream's Stop button: "Stopped: … You stopped it." is
   announced and focus lands on the "Active streams" heading.
9. Sign out: focus moves to the "Sign in" heading.

Record the screen reader, browser and versions with any problems found.

## Manual test script (Mac app, about 10 minutes)

With VoiceOver running (Command+F5):

1. Open AudioNet. VoiceOver reads the window "AudioNet" and the heading
   "Sign in to AudioNet"; focus is in the server address (or account name)
   field. Tab through: server address, account name, password, name for
   this Mac, Sign In.
2. Clear the account name and press Return: "Enter the server address,
   your account name and password, and a name for this Mac." is announced
   and focus moves to the empty field.
3. Sign in with a wrong password: "Signing in failed: …" is announced.
4. Sign in correctly: "Signed in as … This Mac is online there, not
   sharing its audio …" is announced, then "Online, not sharing its
   audio." Turn on Share This Mac's Audio: "Sharing this Mac's audio in …"
   is announced.
5. In "Your devices", move through the devices: each says its name and
   whether it is online, online but not sharing, or offline. Open one.
6. Tab to "Sound to listen to" and press VO-Space: the menu opens; choose a
   sound with the arrow keys and Return. Do the same for "Play it on", then
   press Listen: "Starting: Listening to …" and "…: connected." are
   announced.
7. Turn on Show Measurements in Settings, then read the stream line and
   its measurements with VO-arrow keys; the measurements are never
   announced by themselves.
8. Press the stream's Stop button (its name includes the stream): "Stopped:
   …" is announced.
9. Press Status Log…: a dialog opens. Read the log with VO-arrow keys line
   by line; Copy says "Status copied." Escape closes it.
10. Close the window (Command+W): VoiceOver says AudioNet is still running
    in the menu bar, and AudioNet is no longer in the Dock or Command-Tab.
    Reach the menu bar item with VO-M twice: it says whether this Mac
    shares its audio and has Open AudioNet, Stop Sharing, Settings… and
    Quit AudioNet.
    Choose Settings…: the Settings window opens in front.
11. Open Settings (Command+comma): every checkbox and button is named, and
    "Check for Updates Now" reports the result in words.

Record the macOS and VoiceOver versions with any problems found.
