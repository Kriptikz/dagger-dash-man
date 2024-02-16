# Dagger Management Dashboard

## Experimental Project Notice

This project is currently **EXPERIMENTAL** and **WIP**. Use at your own discretion.


## Current Features:

- Live tailing of Dagger Wield Node `config.log`.
- Password login authentication. For https, follow the installation [below](#installation)
- Show Wield Node version.
- Show Wield Node service status.
- Management Buttons:
  - Stop Wield Node service
  - Start Wield Node service
  - Update Wield Node Binary and Restart service. New wield node binaries will automatically be 
downloaded while this service is running. Wield Node Version will be updated with 
`**NEW VERSION AVAILABLE` and the new version value. If there is a new version available, 
simply click the `Update Wield Service` button to update to the new version and restart the service.



## Future Features:

- Show last released Wield Node version.
- Management Buttons:
  - Update wield-installer
  - Clear history_db
  - Clear peers_db
  - Install Wield Node/ wield-installer
  - Uninstall Wield Node/ wield-installer


## Installation

- Download the binary from: TODO.
- If you are building from source, install the rust toolchain and use `cargo build --release`
  - You may need to install `libssl-dev` or others in order to build it. The error messages 
from cargo will tell you.

- Once you have the binary, follow the NGINX and DNS setup below to ensure you are 
connecting to the web server securely.

- After finishing the NGINX and DNS setup, read over the Running this web server section.
 
### NGINX and DNS

- Install nginx
```shell
sudo apt update
sudo apt install nginx
```

- Set A Record in DNS configuration
```
Type: A Record
Name: example.com <- Your domain name
Content: 0.0.0.0 <- Your vps public IP
TTL: Auto or Default of 36000
```

- [Install and setup certbot](https://certbot.eff.org/instructions?ws=nginx&os=ubuntufocal)
  - Follow the above guide to install certbot and any dependencies.
  - Use the `sudo certbot --nginx` command to get the certificate.
    - It will ask for an email address.
    - Then it will ask for the domain name and get the certificate.
    - Verify the default nginx website is now showing on your domain and it is secured with https.

- Configure nginx with this web server.

  - open the nginx config:
  ```shell
  sudo vim /etc/nginx/
  ```

  - Edit the `/` location block in the ssl configured server
  - Change this:
  ```
    location / {
        # First attempt to serve request as file, then
        # as directory, then fall back to displaying a 404.
        try_files $uri $uri/ =404;
    }
    ```
    to this:
    ```
    location / {
        proxy_pass http://localhost:3000;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection 'upgrade';
        proxy_set_header Host $host;
        proxy_cache_bypass $http_upgrade;
    }
    ```
    - we also need to tune the configuration for the sse routes.
    ```
    location /stream-logs {
        proxy_pass http://localhost:3000;
        proxy_http_version 1.1;
        proxy_set_header Connection '';
        proxy_buffering off;
        proxy_cache off;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        proxy_read_timeout 24h;
        proxy_connect_timeout 1d;
    }
    ```
    
    - save the file
    - verify the configuration file to make sure there are no syntax errors
    ```shell
    sudo nginx -t
    ```
    - if the test is successfull, reload the nginx service to apply these changes
    ```shell
    sudo systemctl reload nginx
    ```
### Running this web server.
  - Download/Build the binary and run it however you want.

  - TODO: Setting it up as a systemd service.


#### Sudo notes
  - running systemctl commands requires `sudo`. Which means when running this service
if the start, stop, or restart commands are issued the web server will request a
password to authenticate and run that command. This defeats the whole purpose though,
as I want to be able to quickly restart/update from the dashboard. There are two 
ways around this:
    1. run the web server with `sudo`. **- It is not recommended to run a server with 
elvated privileges**
    2. Allow the `dagger` user to manage the wield service. This means as long as
the user running a systemctl command for the wield service will not require `sudo`.
This also keeps the server from doing whatever it wants with `sudo` privileges.

#### Allowing dagger user to manage wield service

  - To allow the `dagger` user to manage the wield service we just need to create a 
`.plka` file for PolicyKit. PolicyKit should be already installed if you are using
Ubuntu.


  - Create a new `.plka` file in the appropriate directory. You can use nano or 
another text editor, I will just use vim.:
  ```shell
  sudo vim localauthority/50-local.d/99-wieldnode.pkla
  ```

  - Paste the following into the file:
```
[Allow dagger to manage wield]
Identity=unix-user:dagger
Action=org.freedesktop.systemd1.manage-units;org.freedesktop.systemd1.manage-unit-files
ResultActive=yes
ResultInactive=yes
ResultAny=yes
```
  
  - Save and quit.

  - You can verify the setup by simply running the systemctl restart command for 
the wield service. It should not ask for a password when managing the wield service
with systemctl commands as the `dagger` user anymore.
  ```shell
  systemctl restart wield
  ```

  - When running the dashboard web server as the `dagger` user, it will no longer 
need to be elevated with `sudo` or required a password when running the start, 
stop, and restart commands.



#### Web server config

  - The web server will automatically create a config at `/home/dagger/.config/dagdashman/config.toml`
With a newly generated random password. You can use this password to login to the dashboard.
If you change the password in the config file, you need to stop and start the web server so
it will load the new config.



