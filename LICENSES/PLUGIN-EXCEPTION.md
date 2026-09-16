# Mondrian Plugin Additional Permission v1.0
# Mondrian 插件附加许可 v1.0

Shaloong · contact@shaloong.com

## 1. Application / 适用范围

This is an additional permission under section 7 of GNU AGPL version 3 (AGPL). The Licensor identified in [LICENSE](../LICENSE) applies it to first-party Mondrian program code provided under AGPL-3.0-or-later in the accompanying source tree, to the extent the Licensor owns or is expressly authorized to grant these rights. Such code, together with modifications whose rightsholders retain or adopt this permission, is the **Covered Software**. Separately licensed material and third-party material without such authorization are excluded. Contributors retain their copyright. This permission supplements use under AGPL version 3; it does not interpret or automatically supplement a later AGPL version.

本文件是 GNU AGPL 第 3 版（AGPL）第 7 条下的附加许可。[LICENSE](../LICENSE) 所列许可方，在其享有权利或获得明确授权的范围内，将本许可适用于随附源码树中按 AGPL-3.0-or-later 提供的 Mondrian 自有程序代码。该代码及其权利人保留或采用本许可的修改，统称**受覆盖软件**。另有独立许可的材料及未经相应授权的第三方材料不在本授权内。贡献者保留著作权。本许可补充依 AGPL 第 3 版进行的使用，不解释或自动补充后续 AGPL 版本。

## 2. Independent extensions / 独立扩展

An **Independent Extension** is separately identifiable code written to extend, customize or interoperate with the Covered Software, without copying or adapting its copyright-protected implementation, except material permitted under section 3. Dependence on Mondrian, access to internal or public interfaces, replacement or interception of functionality, competition with official features, and integration into the same source tree or executable do not by themselves disqualify an extension. Qualification does not depend on a designated API list, loading mechanism, separate compilation, name, branding, size or revenue.

**独立扩展**指为扩展、定制受覆盖软件或与其互操作而编写、可单独识别的代码；除第 3 条允许的材料外，不包含从受覆盖软件复制或改编的受著作权保护的实现。依赖 Mondrian、访问内部或公开接口、替换或拦截功能、与官方功能竞争、集成于同一源码树或可执行文件，这些事实本身均不使扩展失去资格。资格不取决于指定 API 清单、加载机制、单独编译、名称、品牌、规模或收入。

The adopting rightsholders permit you to develop, reproduce, modify, combine, link, load, run and convey Independent Extensions with the Covered Software, including in network services. You may choose the license terms for the Independent Extension and whether to charge for it, without registration or separate approval. AGPL conditions that would require the Independent Extension itself to be licensed under AGPL, disclosed in source form or generally redistributable solely because of the combination are waived, including for conveying and AGPL section 13 network interaction, subject to section 4 below. The Independent Extension's own source and extension-only private build materials are excluded from Corresponding Source required for the combination.

采用本许可的权利人允许你开发、复制、修改独立扩展，将其与受覆盖软件组合、链接、加载、运行及交付，包括用于网络服务。你可自主选择独立扩展的许可条款并决定是否收费，无需登记或另行审批。在遵守下述第 4 条的前提下，本许可豁免仅因组合而要求独立扩展本身采用 AGPL、披露源码或允许一般性再分发的 AGPL 条件，包括交付及 AGPL 第 13 条网络交互场景。组合须提供的对应源码不包括独立扩展自身的源码及仅用于该扩展的私有构建材料。

## 3. SDK and interface material / SDK 与接口材料

The first-party SDK file `crates/mondrian-effects/src/plugin_sdk.rs` and first-party fenced source/configuration examples in Markdown files under `docs/plugins/` are additionally available under [MIT](MIT-SDK.txt). This does not relicense their dependencies, documentation prose, images or third-party examples. Other material is MIT-licensed only where expressly identified as such.

自有 SDK 文件 `crates/mondrian-effects/src/plugin_sdk.rs` 及 `docs/plugins/` 下 Markdown 文件中自有的围栏源码／配置示例，另按 [MIT](MIT-SDK.txt) 提供。本授权不改变其依赖、文档正文、图像或第三方示例的许可。其他材料仅在明确标注时适用 MIT。

To the extent rights are needed, you may reproduce interface declarations, type layouts and function or method signatures from the Covered Software as necessary for interoperability in an Independent Extension and convey them as part of that extension under its chosen terms. This does not authorize copying implementation bodies, algorithms, macros or inline helpers except under an applicable separate license. Normal compilation, including inlining or generic instantiation, does not disqualify an Independent Extension; generated copies of Covered Software remain Covered Software subject to section 4.

在需要相应授权的范围内，你可为互操作所必需，在独立扩展中复制受覆盖软件的接口声明、类型布局及函数或方法签名，并作为扩展的一部分按其所选条款交付。除适用的独立许可另有授权外，本条不授权复制实现体、算法、宏或内联辅助实现。正常编译，包括内联或泛型实例化，不使独立扩展失去资格；生成的受覆盖软件副本仍属受覆盖软件，适用第 4 条。

## 4. Retained obligations / 保留义务

The Covered Software and changes to its code remain subject to AGPL, including applicable copyright and license notices, Corresponding Source and Installation Information requirements. Those obligations arise in the circumstances specified by AGPL; this permission does not require all private modifications to be published. Moving, renaming, translating, adapting or wrapping Covered Software does not make it an Independent Extension. Adding independently written code does not exempt the pre-existing code or modifications to it.

受覆盖软件及对其代码的修改仍适用 AGPL，包括适用的版权及许可声明、对应源码和安装信息要求。这些义务在 AGPL 规定的情形下产生；本许可不要求公开所有私人修改。将受覆盖软件移动、改名、翻译、改编或包装，不使其成为独立扩展。增加独立编写的代码不豁免原有代码及对原有代码的修改。

When Corresponding Source is required, provide the Covered Software's source, modifications and required build materials. If rebuilding a statically linked combination requires extension artifacts, also provide the necessary non-source artifacts, interface information and instructions, under terms permitting recipients to rebuild, relink and run the combination with their modified Covered Software. Extension source need not be disclosed. No term for the extension or combined product may restrict recipients' applicable rights in the Covered Software or prevent these permitted activities. Applicable Installation Information requirements remain in force.

须提供对应源码时，应提供受覆盖软件的源码、修改及所需构建材料。重新构建静态链接组合需要扩展产物时，还须提供必要的非源码产物、接口信息和说明，并允许接收者使用其修改后的受覆盖软件重新构建、链接和运行组合，无须披露扩展源码。扩展或组合产品的条款不得限制接收者对受覆盖软件的适用权利，或阻止上述获准活动。适用的安装信息要求继续有效。

## 5. Other rights and continuity / 其他权利及持续效力

This permission does not waive third-party terms or grant rights in third-party code, extensions owned by others, trademarks or confidential material. Adopting rightsholders grant a non-exclusive, worldwide, royalty-free patent license under claims they control that are necessarily infringed by their Covered Software in the combinations permitted here; independent extension inventions and third-party patents are excluded. Rights available under law or other licenses remain unaffected. A use outside this permission may still be authorized by AGPL, another license or a separate commercial agreement.

本许可不豁免第三方条款，也不授予第三方代码、他人扩展、商标或保密材料的权利。采用本许可的权利人，就其控制且在本许可允许的组合中必然因其受覆盖软件而实施的专利权利要求，授予非独占、全球、免许可费的专利许可；独立扩展发明及第三方专利除外。法律或其他许可提供的权利不受影响。不属于本附加许可的使用，仍可能依据 AGPL、其他许可或另行商业协议获得授权。

Recipients may remove this permission under AGPL section 7; other rightsholders need not adopt it for their additions. Later changes to these notices do not revoke earlier grants. Termination and reinstatement follow AGPL. English controls this permission; Chinese is a translation, subject to mandatory applicable law.

接收者可依 AGPL 第 7 条移除本许可；其他权利人无需为其新增内容采用本许可。后续声明变更不撤销既有授权。终止和恢复依 AGPL 处理。本许可英文文本优先，中文为对照翻译；适用强制性法律不受影响。
