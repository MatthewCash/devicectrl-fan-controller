# devicectrl-fan-controller

Device implementation to communicate with FanLamp Pro V2 ceiling fans.

## Configuration

Example:

```json
{
    "device_id": "fan-controller",
    "server_addr": "10.0.2.1:8895",
    "server_public_key_path": "/etc/devicectrl-fan-controller/server_public.der",
    "private_key_path": "/etc/devicectrl-fan-controller/fan-contorller_private.der",
    "hci_device": 0
}
```

## Running

Simply execute:

`CONFIG_PATH=config.json cargo run`

Or use the provided systemd service:

```bash
cargo build --release

sudo install -m 755 ./target/release/devicectrl-fan-controller /usr/local/bin
sudo install -m 644 devicectrl-fan-controller.service /etc/systemd/system/

sudo systemctl daemon-reload
sudo systemctl enable --now devicectrl-fan-controller
```
