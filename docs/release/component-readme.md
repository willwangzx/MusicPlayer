# MusicPlayer Apple Music 下载组件 — 使用说明

这是 MusicPlayer 的可选下载组件。只有想用 Apple Music 下载功能时才需要安装;
本地播放不需要它,不安装不影响播放。

组件内容:已打补丁的 musicdl 2.13.7、自带完整 Python 3.13 运行环境
(无需在你电脑上另外安装 Python),以及 ffmpeg / N_m3u8DL-RE / Bento4
(mp4decrypt) 工具。所有关键文件都用 SHA-256 校验锁定,安装时自动验证。

## 前置条件

- 已解压 MusicPlayer 主程序(另一个压缩包)。
- 你需要一个 Apple Music 订阅账号,并自行获取 media-user-token。
  组件不包含、也不能替你获取账号凭证。

## 安装(一次性)

1. 把组件包解压到一个固定位置,例如 `D:\Apps\MusicPlayer-AAC`:
   - 路径可以包含空格或中文;
   - 不要放在 `D:\Music` 里面(避免被曲库扫描);
   - 解压后不要改动、移动或删除里面的文件(校验会发现不一致)。
2. 在该文件夹中打开 PowerShell,运行:

   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File .\Install-AppleMusicComponent.ps1
   ```

   脚本会先校验全部组件文件(与锁定哈希不符会拒绝安装),然后注册组件。
   文件不会被复制到别处,播放器只记住这个文件夹的位置。
3. (可选)配置 Apple Music 凭证 —— 在同一 PowerShell 窗口运行:

   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File .\Install-AppleMusicComponent.ps1 -ConfigureCredentials
   ```

   按提示粘贴 media-user-token(输入时不回显;以 Windows 当前用户 DPAPI
   加密保存在本组件文件夹的 `.config` 内,不会上传到任何地方)。
4. 重启 MusicPlayer → 打开 Apple Music 页 → "Check account" 显示已连接即可。

media-user-token 的获取方式请自行搜索相关教程;不同账号/地区可用性不同,
组件不保证下载一定成功。

## 校验与卸载

- 只校验不注册:

  ```powershell
  powershell -NoProfile -ExecutionPolicy Bypass -File .\Install-AppleMusicComponent.ps1 -VerifyOnly
  ```

- 卸载:删除本组件文件夹;再删除
  `%LOCALAPPDATA%\MusicPlayer\state\downloader-config.json` 取消注册。
  (不删注册文件也没关系,播放器会提示组件不可用,本地播放不受影响。)

## 许可

- 组件内的 musicdl 为 PolyForm Noncommercial 1.0.0 许可(仅限非商业用途);
  另含 GPL 许可的 ffmpeg、Bento4 等第三方软件。
  许可与源码链接见 COMPONENT-NOTICES.txt。
- 本组件为私人分享,请勿公开传播。
