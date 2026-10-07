# Pairing a phone, and firewalls

Pair phone (in the desktop app) and `squigl-cli --webrtc` let a phone's browser stream
its camera to this computer, with no app on the phone. The phone opens a page served by
this computer, then sends the video straight to it. Both steps are connections **into**
this computer, so a firewall that blocks incoming connections stops pairing.

## What has to get through

| What | Protocol | Port | When |
|---|---|---|---|
| The page and the pairing request | TCP (HTTPS) | 8443, or a free port if 8443 is taken (the address shows it) | When the phone opens the address |
| The video | UDP | A new free port for each phone | After Start streaming |

Each runs only while needed: the desktop app listens only while the Pair phone dialog is
open, and the video port only while a phone streams. Both are on the addresses the
dialog offers (the local network, and Tailscale, ZeroTier or Nebula), never on every
interface.

**Telling which one is blocked:**
- The phone's browser can't open the address at all (it times out): the TCP port is
  blocked, or the phone is on a different network.
- The page opens, but after Start streaming its log shows `connection: checking` and
  then `connection: failed`: the UDP video is blocked.

## Windows

The first time Squigl listens for a phone, Windows Defender Firewall asks: *"Windows
Defender Firewall has blocked some features of this app"*, with a tick box each for
**Private networks** and **Public networks**.

- Choose **Allow access**. This needs an administrator's approval.
- The rule covers the app, so it lets in both the page and the video.
- **The network's profile matters.** Windows treats a newly joined Wi-Fi network as
  *Public* unless told otherwise. If only Private networks was ticked, a phone on a
  Public network cannot connect. Either:
  - make the network Private: Settings → Network & internet → Wi-Fi (or Ethernet) →
    the network → Network profile type → **Private network** (right for home and
    trusted networks); or
  - tick Public networks as well, on a network you trust.
- **Cancel blocks.** Choosing Cancel at the prompt adds a rule that blocks the app, and
  Windows does not ask again. To undo it, open Control Panel → Windows Defender
  Firewall → **Allow an app or feature through Windows Defender Firewall**. Then choose
  Change settings, find Squigl, and tick Private (and Public if needed).

*To check in the Windows VM:* the prompt's wording and defaults on Windows 11, and which
profile Windows gives the Tailscale adapter.

## macOS

*From Apple's documentation; no tester has confirmed these on a Mac yet.*

- **The application firewall** is off by default (System Settings → Network →
  Firewall). When it is on, the first time Squigl listens, macOS asks *"Do you want the
  application "Squigl" to accept incoming network connections?"*. Choose **Allow**.
  - A signed app keeps the answer.
  - An unsigned build may be asked again after each rebuild.
  - "Block all incoming connections" stops pairing altogether.
- **Local network privacy** (macOS 15 and later) asks before an app talks to devices on
  the local network. Checking the video connection means Squigl sends packets to the
  phone, so expect *"Allow "Squigl" to find devices on local networks?"* at the first
  pairing.
  - The app's `Info.plist` gives the reason shown in that prompt
    (`NSLocalNetworkUsageDescription`); Phase 6's packaging adds it.
  - A refusal can be changed in System Settings → Privacy & Security → Local Network.

## Linux

Most desktops let local connections in as installed:

- **Ubuntu:** `ufw` is installed but off. If you have turned it on, allow the phone's
  network, since the video port changes per phone. For example, for a home network on
  192.168.1.x:

  ```
  sudo ufw allow from 192.168.1.0/24
  ```

  Over Tailscale, allow its interface instead: `sudo ufw allow in on tailscale0`.
- **Fedora Workstation:** `firewalld`'s default FedoraWorkstation zone already allows
  incoming TCP and UDP on ports 1025-65535, which covers both.
- **Fedora with a stricter zone,** or other `firewalld` setups: allow the TCP port and
  the high UDP ports from the network, e.g.

  ```
  sudo firewall-cmd --add-port=8443/tcp --add-port=1025-65535/udp
  ```

  (add `--permanent` to keep it).

## The phone on another network

A phone on mobile data, or on a guest Wi-Fi that keeps devices apart, cannot reach this
computer's local address. Either:

- **connect both to the same network**, for instance the phone's own hotspot; or
- **install Tailscale (or ZeroTier, or Nebula) on both**, then pair at the address the
  dialog offers for it.

  Tailscale's access controls (ACLs) must let the phone reach this computer; the
  default policy allows it. Tailscale also has its own HTTPS certificates, which spare
  the phone the browser's warning. Turn them on in Settings → Phone pairing; doing so
  publishes this computer's tailnet name in public certificate logs.
