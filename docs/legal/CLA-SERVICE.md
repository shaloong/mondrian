# CLA Assistant 接入与签署记录 / CLA Assistant procedure

状态：Gist 已公开并已关联 CLA Assistant；签署页已展示。尚未完成真实签署及导出验收；CLA 合并规则已创建但处于停用状态。在线入口：https://cla-assistant.io/shaloong/mondrian 。在上线验收完成前，不将外部贡献视为已完成授权核验。

Status: the public Gist is linked and the signing page is displayed. Execution/export testing remains incomplete and the prepared CLA ruleset is disabled. Entry: https://cla-assistant.io/shaloong/mondrian . Do not treat external contributions as authority-verified solely from this setup.

公开正文：[CLA Gist](https://gist.github.com/shalomwang/ffaa6f1c3c54b09b1e9f4b84c3097cae) · revision `670475906da5e878495cf5aa50c9930e506466f5` · SHA-256 `7442c81ad95361aba24ac7e65e3a3fc8ae3527aaf295ab3bde237a5496b3a416`。仓库正文与该修订字节一致。

Published agreement: the Gist revision and SHA-256 above match the repository text byte for byte.

签署前告知见 [贡献者签署隐私告知](CLA-PRIVACY.md)。该文件准备完成不代表线上展示、依法需要的单独同意及实际数据处理已经验收。

See the [signing privacy notice](CLA-PRIVACY.md). Preparing this document does not establish that online presentation, any required separate consent or actual processing has been verified.

协议正文不指定服务品牌或存档技术；当前操作方案为下述 CLA Assistant。更换服务本身不要求重新签署既有协议，但须保留可核实的旧签署记录，并落实新服务适用的隐私告知。平台若无法导入既有记录，其检查状态需另行处理，不能据此认定旧授权无效。

The agreement does not prescribe a service brand or storage technology; the current implementation uses CLA Assistant below. Migration alone does not require re-execution, but verifiable prior records must be retained and applicable privacy notices for the new service provided. If a platform cannot import earlier records, its check status requires separate handling; that limitation does not invalidate existing grants.

## 服务与协议 / Service and agreement

使用 SAP 提供的 [cla-assistant.io](https://cla-assistant.io/) 在线服务，配置项目自己的完整 CLA，不能以平台示例协议替代。平台认证 GitHub 账户、记录同意、更新 PR 状态；不替项目判断权利归属或授予第三方商业许可。

Use the SAP-hosted service with the complete project CLA, not a provider sample agreement. Authentication, assent records and PR status do not establish ownership or grant third-party commercial rights.

## 管理员启用步骤 / Administrator activation

1. 用仓库管理员账号登录服务，审查实际请求的权限，仅关联 Mondrian 仓库。不要把访问令牌写入仓库。
2. 将发布的 `CLA.md` 完整正文复制到专用 Gist。上传稿应与仓库正文一致；当前正文不依赖相对流程链接。保存 Gist ID、revision、确切签署正文 SHA-256、仓库 commit、版本及发布日期；旧版本保留，不覆盖签署证据。
3. 在关联仓库前审查当前服务隐私声明、处理者联系方式、数据区域、字段可见性及导出内容。官方说明目前为欧洲 Azure 数据库；不能据此推断不存在其他处理地点。提供实际处理和必要跨境告知，依法需要同意时单独取得；完成前不启用签署，不向服务传输或收集贡献者个人信息（包括账户标识），也不上传法定姓名、邮件或企业文件。私密邮件途径保留。
4. 首先使用最少账户及同意数据。保存首次签署关联 PR；仅在实际需要时补充身份、组织授权或其他历史贡献记录，并与账户关联；不要将敏感信息放在 Gist metadata、PR 评论或公开签署列表。若启用自定义字段，必须先核验其可见性和导出行为。
5. 用测试 PR 验证：未签署不能合并；签署确切版本后服务状态更新；不同作者、共同作者及追加提交均受检查；协议变更需重新同意；fork PR 不取得写入密钥。将实际观测到的服务检查设为目标分支必需检查并核验预期来源；不要猜测检查名称或仅凭工作流文件声称已启用。
6. 在线明确同意依 CLA D3 使协议成立，无需维护者另行签署或接受。普通个人贡献以账户认证、同意记录和 PR 检查为常规流程；涉及组织权利、来源疑问或未覆盖的授权范围时再补充核验。平台检查不证明第三方授权。依赖更新机器人可按服务支持配置例外；例外不代替对实际提交材料的许可审查。
7. 将本节状态更新为已启用，公开实际入口、协议版本、正文哈希、Gist revision 和启用日期。验收证据与账户授权记录私下保存。管理员授权或远程分支规则未完成时，不宣称强制 CLA 检查已生效。

Administrators must bind the exact agreement and version, validate privacy and access before collection, test unsigned/signed/multi-author/changed-version PRs, enforce the actual service check on protected branches, and publish the verified activation record. Online assent forms the agreement under CLA D3 without further maintainer acceptance; ordinary individual contributions use account authentication, assent records and PR checks. Supplementary review is needed for organizational rights, provenance doubts or uncovered scope. Supported bot exceptions do not clear rights in submitted material. Do not claim activation from local configuration alone.

## 私密记录与变更 / Private records and changes

保存各签署版本的完整正文和平台同意记录，定期导出 CSV 并核对导出字段。后续普通 PR 使用服务检查，无需逐 PR 下载记录或人工复签；发现记录缺失或授权疑问时再补充。必要的组织授权等附件私下保存，按 CLA A7 限制访问和保存期限。公开仓库不保存签署 CSV。

Retain the full text of each signed version and platform assent records; periodically export CSVs and check the exported fields. Ordinary later PRs use the service check, without per-PR downloads or manual re-execution. Supplement missing evidence or uncertain authority as needed. Keep necessary organizational authority attachments private, with access and retention governed by CLA A7. Do not commit signer CSVs.

启用后冻结签署 Gist，日常说明更新放在仓库流程文档；平台可能因 Gist 更新触发重签，即使版本号未变。协议条款变更发布新版本，保留旧协议及旧签署；平台自动要求重新签署不意味着追溯扩大旧授权。停服或迁移时先保存记录，再启用替代流程。其他平台接受的协议不得默认为本项目 CLA。

Freeze the signing Gist after activation and place routine guidance updates in repository procedure documents; a Gist update may trigger re-signing even without a version-number change. Publish a new version for changed terms and retain old grants and records. Re-signing does not retroactively expand earlier grants. Export evidence before migration; signing another project's CLA does not execute this one.

## 官方资料 / Official references

- [CLA Assistant 功能、版本、导出及数据存储说明](https://github.com/cla-assistant/cla-assistant)
- [GitHub 必需状态检查](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets)

平台不提供本项目协议的法律效力保证；启用检查证明的是流程工作正常，不是每份贡献已完成法律审查。

The platform does not warrant the legal effectiveness of this project's agreement; a functioning check is process evidence, not a legal review of each Contribution.
