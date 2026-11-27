# DOSO OTA 管理台

面向研发与运维的可视化 OTA 发布与设备管理界面，零第三方依赖（仅需 Python 3），直接复用 `tools/deploy/doso-ota-ops.py` 的打包、上传与发版逻辑。

## 快速启动

- Windows：直接双击仓库根目录的 `打开DOSO-OTA管理台.bat`，会自动在浏览器打开 `http://127.0.0.1:8190/`。
- 命令行：
  ```bash
  python tools/ota-console/server.py --open
  ```

## 配置说明

首次打开后进入「云端配置」填写一次：
- 云端服务地址（默认 `https://api.dosorobotics.com/ota`）
- 发布令牌（Bearer Token）
- 对象存储桶、节点及 AccessKey

凭据保存在本机用户目录 `~/.doso-ota/console.env`（不进入 Git 仓库），界面不会回显明文密钥。

## 测试

```bash
python -m unittest -v tools/ota-console/test_server.py
```
