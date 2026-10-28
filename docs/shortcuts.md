# Shortcut setup

Quick Translate uses one global shortcut, **Ctrl+Alt+C** by default. Pressing it shows the
translation popup for the current selection. Pressing it again hides the popup.

The daemon picks one of these mechanisms when it starts. Settings shows the active one.

1. **System shortcut (portal).** If the desktop's xdg-desktop-portal provides the
   `org.freedesktop.portal.GlobalShortcuts` interface, Quick Translate registers the
   `translate-selection` shortcut there and asks for `Ctrl+Alt+C`. The desktop can assign a
   different key. Settings shows the key that was actually assigned.
2. **Compositor binding.** Without the portal, you bind a key in your compositor to run
   `quick-translate --toggle`. Settings shows a ready-to-paste snippet for your compositor,
   with a Copy button. Quick Translate never edits compositor or desktop config files.
3. **Input-device fallback (opt-in).** Use this only if neither of the above works. See
   [Input-device fallback](#input-device-fallback).

`quick-translate --status` prints the detected GlobalShortcuts portal version.

## Desktops with the shortcut portal

### KDE Plasma 6

On first start, Plasma shows a dialog that proposes `Ctrl+Alt+C` for "Translate the selected
text". Accept it, or pick another key there. To change it later, click **Change shortcut** in
Quick Translate's settings. This opens Plasma's shortcut dialog when the portal reports
GlobalShortcuts version 2 or later. With older versions, open System Settings > Keyboard >
Shortcuts and look for Quick Translate.

The binding survives restarts. Quick Translate binds again on every start (needed on some
Plasma 6 releases), and the portal keeps the key you chose.

### GNOME 48 and later

GNOME 48 added the GlobalShortcuts portal. On first start, GNOME asks you to confirm the
shortcut. Change it later with **Change shortcut** in settings, or in Settings > Keyboard >
View and Customize Shortcuts > Applications.

On GNOME releases before 48, the portal is missing. Use the [GNOME](#gnome) custom-shortcut
instructions below.

### Hyprland

xdg-desktop-portal-hyprland provides the GlobalShortcuts portal, but it does not assign keys
on its own. Bind the registered shortcut with the `global` dispatcher in
`~/.config/hypr/hyprland.conf`:

```ini
bind = CTRL ALT, C, global, io.github.quicktranslate.QuickTranslate:translate-selection
```

`hyprctl globalshortcuts` lists the registered shortcuts if the name differs. If you do not
run the Hyprland portal, use the [native bind](#hyprland-1) below instead.

## Compositor bindings

The snippets below assume the AppImage is at
`/home/you/Applications/QuickTranslate-x86_64.AppImage`. Settings shows the snippet with
your real path: the AppImage path when you run the AppImage, `quick-translate` when it is on
your `PATH` (for example from the Nix flake), or else the full path of the binary. Paths with
spaces are quoted in the format each config file expects.

### Sway

Add this line to `~/.config/sway/config`, then run `swaymsg reload`:

```sh
bindsym Ctrl+Alt+c exec /home/you/Applications/QuickTranslate-x86_64.AppImage --toggle
```

### Hyprland

Native bind, for setups without the Hyprland portal. Add this line to
`~/.config/hypr/hyprland.conf`. Hyprland reloads it automatically:

```ini
bind = CTRL ALT, C, exec, /home/you/Applications/QuickTranslate-x86_64.AppImage --toggle
```

### niri

Add this line inside the `binds { }` section of `~/.config/niri/config.kdl`. niri reloads it
automatically:

```kdl
Ctrl+Alt+C { spawn "/home/you/Applications/QuickTranslate-x86_64.AppImage" "--toggle"; }
```

### river

Add this line to your river init script (`~/.config/river/init`), then run it once or
restart river. This uses `riverctl` (river 0.3 and river-classic):

```sh
riverctl map normal Control+Alt C spawn '/home/you/Applications/QuickTranslate-x86_64.AppImage --toggle'
```

### labwc

Add this inside the `<keyboard>` section of `~/.config/labwc/rc.xml`, then run
`labwc --reconfigure`:

```xml
<keybind key="C-A-c">
  <action name="Execute" command="/home/you/Applications/QuickTranslate-x86_64.AppImage --toggle" />
</keybind>
```

### Wayfire

Add these lines to the `[command]` section of `~/.config/wayfire.ini`. Create the section if
it is missing, and make sure `command` is listed in `plugins` under `[core]`. Wayfire reloads
the file automatically:

```ini
binding_quick_translate = <ctrl> <alt> KEY_C
command_quick_translate = /home/you/Applications/QuickTranslate-x86_64.AppImage --toggle
```

### COSMIC

Open Settings > Keyboard > Keyboard Shortcuts > Custom Shortcuts > Add Shortcut. Name it
"Quick Translate", paste this command, and press Ctrl+Alt+C:

```text
/home/you/Applications/QuickTranslate-x86_64.AppImage --toggle
```

### KDE Plasma

Only needed when the portal is missing, for example on Plasma 5. Open System Settings >
Keyboard > Shortcuts > Add New > Command or Script. Paste this command, then set Ctrl+Alt+C
as its shortcut:

```text
/home/you/Applications/QuickTranslate-x86_64.AppImage --toggle
```

### GNOME

Only needed before GNOME 48. Open Settings > Keyboard > View and Customize Shortcuts >
Custom Shortcuts > Add Shortcut. Name it "Quick Translate", paste this command, and set
Ctrl+Alt+C:

```text
/home/you/Applications/QuickTranslate-x86_64.AppImage --toggle
```

### Other environments

Bind Ctrl+Alt+C (or any key you like) in your compositor's or desktop's shortcut settings to
run this command:

```text
/home/you/Applications/QuickTranslate-x86_64.AppImage --toggle
```

## Input-device fallback

If you can use neither the portal nor a compositor binding, Quick Translate can watch the
keyboard devices in `/dev/input` for Ctrl+Alt+C (left or right Ctrl and Alt, no other
modifiers). It is off by default. Turning it on in settings shows a warning first, because
**in this mode the app can read all keyboard input**. The mode starts only after you confirm.
Quick Translate only reads the devices and never grabs them, so other apps still get every key.

Reading `/dev/input/event*` usually requires membership in the `input` group:

```sh
sudo usermod -aG input "$USER"
```

Log out and back in afterwards. If the devices are not readable, settings says that
permission is missing and the mode stays off. Keyboards plugged in after the daemon started
are picked up on the next start.
