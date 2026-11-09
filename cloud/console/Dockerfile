ARG NGINX_IMAGE
FROM ${NGINX_IMAGE}

COPY nginx.conf /etc/nginx/nginx.conf
COPY dist/ /usr/share/nginx/html/
COPY licenses/ /usr/share/nginx/html/licenses/
COPY manifest.json /usr/share/nginx/html/licenses/build.json
USER nginx
EXPOSE 8080
ENTRYPOINT ["nginx", "-g", "daemon off;"]
