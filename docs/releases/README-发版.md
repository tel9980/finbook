# 发版怎么发（备用手册）

正常情况 `git tag vX.Y.Z && git push origin vX.Y.Z` 就够了，`release.yml` 会建 Release、传附件。

**这一页是 Release job 红了的备用手册。** v1.8.81 发版时用过一次，因为 GitHub 的
`releases/assets` 接口连续报 500/502，把 Release 卡成了 draft。

---

## 先判断是哪种红

打开 Release job 的日志，看最后一个成功步骤：

| 日志里看到 | 含义 | 怎么办 |
|---|---|---|
| `Verify tag & tests` 就失败 | 版本号/tag/CI 不对 | 按报错改，别走备用流程 |
| 三个 `Desktop/Server` 打包 job 失败 | 编译或打包问题 | 看具体 job；与发布无关的偶发（如 mingw apt）重跑即可 |
| 打包成功，`Publish` 里出现 `HTTP 500/502` | **GitHub 接口不稳** | 走下面「手工补发」 |

判断接口是否恢复（不用 token 也能看仓库公开信息）：

```bash
node -e 'fetch("https://api.github.com/repos/tel9980/finbook").then(r=>console.log(r.status))'
# 403 = 频率限制，200 = 正常；500/502 = 还没恢复，等 20 分钟再试
```

---

## 手工补发四步

前提：tag 已经推上去、该 SHA 的 CI 是绿的（否则产物对不上）。

### 1. 清掉半截 Release

半截 Release 通常是 **draft** 且只挂了一两个附件。它是「重跑永远失败」的元凶——
tag 被它占住了，`gh release create` 每次都报已存在。

```bash
TOK=$(cat /root/.dsh/git-token)
node -e '
const tok=process.argv[1];
const h={headers:{Authorization:"Bearer "+tok,Accept:"application/vnd.github+json","User-Agent":"finbook"}};
(async()=>{
  const arr=await (await fetch("https://api.github.com/repos/tel9980/finbook/releases?per_page=10",h)).json();
  for(const x of arr.filter(x=>x.draft)) console.log(x.id, x.tag_name, x.created_at);
})();' "$TOK"
```

看到目标 tag 是 draft 后删掉它（只删 Release，**不动 tag**）：

```bash
node -e '
const [tok,id]=process.argv.slice(1);
(async()=>{
  const r=await fetch("https://api.github.com/repos/tel9980/finbook/releases/"+id,
    {method:"DELETE",headers:{Authorization:"Bearer "+tok,"User-Agent":"finbook"}});
  console.log("delete ->", r.status);   // 204 = 成功
})();' "$TOK" <Release-id>
```

### 2. 把 CI 的构建产物取下来

不用本地编译 —— CI 的 `build-linux` job 已经编好了，直接下载：

```bash
cd /tmp && rm -rf rel && mkdir rel && cd rel
node -e '
const tok=process.argv[1];
const h={headers:{Authorization:"Bearer "+tok,Accept:"application/vnd.github+json","User-Agent":"finbook"}};
(async()=>{
  const arr=await (await fetch("https://api.github.com/repos/tel9980/finbook/actions/runs?per_page=20",h)).json();
  // 取目标 SHA 的那次 CI run
  const run=arr.workflow_runs.find(r=>r.name==="CI" && r.head_sha==="<目标SHA>");
  const a=await (await fetch(`https://api.github.com/repos/tel9980/finbook/actions/runs/${run.id}/artifacts`,h)).json();
  const art=a.artifacts.find(x=>x.name==="finbook-linux-x64");
  const buf=Buffer.from(await (await fetch(art.archive_download_url,{headers:h.headers,redirect:"follow"})).arrayBuffer());
  require("fs").writeFileSync("art.zip",buf);
})();' "$TOK"
```

`art.zip` 里是 `finbook`（桌面 Linux）和 `finweb`（服务器）。**核对版本号**，
别把旧产物贴上去了：

```bash
python3 -c "
import zipfile,re
d=zipfile.ZipFile('art.zip').read('finbook')
print(re.search(rb'1\.8\.\d+',d).group(0))
"
```

Windows 的 exe 若也没了，本地交叉编译一次（这是唯一需要本地编译的例外）：

```bash
cd /workspace/finbook
export PATH=/workspace/.cache/rust/cargo/bin:$PATH
cargo build --release --locked --target x86_64-pc-windows-gnu -p finbook   # 约 4 分钟
```

### 3. 组包

目录结构与 `release.yml` 保持一致，否则用户解压出来的东西不一样。

```bash
cd /tmp/rel && mkdir -p dist/x dist/server dist/docs-releases
python3 -c "
import zipfile
z=zipfile.ZipFile('art.zip')
open('dist/x/finbook','wb').write(z.read('finbook'))
open('dist/x/finweb','wb').write(z.read('finweb'))
"
cp /workspace/finbook/docs/新手使用说明.txt dist/x/
cp /workspace/finbook/docs/小白日常操作教程.md dist/x/
cp /workspace/finbook/docs/老账搬迁操作步骤.md dist/x/
cp "/workspace/finbook/docs/恒亮氧化加工厂演示账.fbk" "dist/x/示例账套.fbk"
chmod +x dist/x/finbook dist/x/finweb
cp /workspace/finbook/docs/releases/<版本>.md dist/docs-releases/

# 桌面 Linux（exe 存在时把它放进 dist/x 一起打进包）
tar -C dist/x -czf dist/finbook-<版本>-linux-x86_64.tar.gz .

# 服务器
cp dist/x/finweb dist/server/ && cp -r /workspace/finbook/crates/finweb/static dist/server/static
rm -f dist/server/static/util.test.js && cp /workspace/finbook/deploy/finweb.service dist/server/
tar -C dist/server -czf dist/finweb-<版本>-linux-x86_64.tar.gz .

# Windows zip：用 python 的 zipfile，不要用 zip 命令
# （容器里没装 zip；且中文文件名用 -j / 系统 zip 容易乱码，保持包内一层目录）
```

### 4. 建 Release 并传附件

```bash
cd /tmp/rel/dist && sha256sum ./*.tar.gz ./*.zip > SHA256SUMS && sha256sum -c SHA256SUMS
```

```bash
# 建 Release（正文用发布说明）
env -u http_proxy -u https_proxy node -e '
const [tok,ver]=process.argv.slice(1); const fs=require("fs");
(async()=>{
  const body={tag_name:"v"+ver,name:"FinBook v"+ver,
    body:fs.readFileSync("/workspace/finbook/docs/releases/v"+ver+".md","utf8"),
    draft:false,prerelease:false};
  const r=await fetch("https://api.github.com/repos/tel9980/finbook/releases",
    {method:"POST",headers:{Authorization:"Bearer "+tok,"User-Agent":"finbook","Content-Type":"application/json"},body:JSON.stringify(body)});
  console.log(r.status, (await r.text()).slice(0,120));
})();' "$TOK" <版本>
```

```bash
# 传附件（注意：uploads.github.com 必须绕过代理，否则 TLS 被掐断）
env -u http_proxy -u https_proxy node -e '
const [tok,ver]=process.argv.slice(1); const fs=require("fs");
(async()=>{
  const j=await (await fetch("https://api.github.com/repos/tel9980/finbook/releases/tags/v"+ver,
    {headers:{Authorization:"Bearer "+tok,"User-Agent":"finbook"}})).json();
  const up=j.upload_url.replace("{?name,label}","");
  for(const f of ["finbook-"+ver+"-linux-x86_64.tar.gz","finweb-"+ver+"-linux-x86_64.tar.gz",
                  "finbook-"+ver+"-windows-x86_64.zip","SHA256SUMS"]){
    const buf=fs.readFileSync(f);
    const r=await fetch(up+"?name="+encodeURIComponent(f),{method:"POST",
      headers:{Authorization:"Bearer "+tok,"User-Agent":"finbook","Content-Type":"application/octet-stream","Content-Length":String(buf.length)},body:buf});
    console.log(f, r.status);
  }
})();' "$TOK" <版本>
```

### 5. 收尾核对

- `https://github.com/tel9980/finbook/releases/tag/vX.Y.Z` 打开是**已发布**状态（不是 draft），4 个附件都在；
- 镜像：三个 tag 同一个 digest
  ```bash
  env -u http_proxy -u https_proxy node -e '
  (async()=>{const t=await (await fetch("https://ghcr.io/token?scope=repository:tel9980/finbook:pull&service=ghcr.io")).json();
  for(const tag of ["<版本>","<主>.<次>","latest"]){
    const r=await fetch("https://ghcr.io/v2/tel9980/finbook/manifests/"+tag,
      {headers:{Authorization:"Bearer "+t.token,Accept:"application/vnd.oci.image.manifest.v1+json"}});
    console.log(tag, r.headers.get("docker-content-digest"));
  }})();'
  ```

---

## 这一节为什么存在

三道防线都懒得加的话，同一个坑会反复踩：

1. `release.yml` 现在**先建空 Release 再逐个传附件**，create 失败不会留下半截状态；
2. 每个附件单独重试 3 次，一个失败不影响已成功的；
3. 最后强制核对「不是 draft 且附件数对得上」，绿但没产物的情况直接判红。
