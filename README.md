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



## Future Features:

- Show last released Wield Node version.
- Management Buttons:
  - Update Wield Node/ wield-installer
  - Clear history_db
  - Clear peers_db
  - Install Wield Node/ wield-installer
  - Uninstall Wield Node/ wield-installer


## Installation
 
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

- Run this web service.



