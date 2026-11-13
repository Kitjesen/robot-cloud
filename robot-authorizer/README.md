# doso-robot-authorizer

本地授权写入服务。只监听 `127.0.0.1:13148`，接受带 Bearer token 的
`POST /v1/cloud/choice`，原子保存 `/var/lib/doso-cloud/choice`，并且只调用
固定的 `systemctl start|stop doso-cloud-link.service`。

请求体：

```json
{"enabled":true}
```

服务不会执行任意命令、不会接收远程网络请求，也不会替代业务账号/设备归属校验。
Token 文件必须是非空的常规文件，不能是符号链接，不能对组或其他用户可写。
