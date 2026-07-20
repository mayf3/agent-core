# Kernel 负面宪法

## 宗旨

这份文档定义了 Kernel 代码库（`src/`）的明确边界。任何修改 Kernel 的 PR 都必须通过以下检查。

---

## 一、Kernel 禁止新增的领域概念

以下概念属于外部 Harness、Planner 或上层编排层，Kernel 不得直接理解或存储：

- `Planner` / `Plan` / `Role` / `Agent`（作为业务角色）
- `AcceptanceKit` / `Bundle` / `Spec` / `Verifier`
- `PublicSpec` / `PrivateVerifier` / `TestCase` / `Fixture`
- `Dashboard` / `ProductType` / `BusinessMetric`
- `Template` / `Strategy` / `ProfileSelector`

## 二、Kernel 允许理解的通用原语

- `Principal` / `Scope` / `Run` / `Intent`
- `Decision` / `Invocation` / `Receipt`
- `ArtifactRef` / `SubjectDigest` / `ReceiptDigest`
- `outcome` / `evidence` / `issuer` / `failure_class`

## 三、每个 Kernel PR 必答的五问

1. **为什么不能只在外部 Harness 完成？**
   - 如果功能可以在外部 Harness 中实现而不降低安全性，它不应该进入 Kernel。

2. **不进入 Kernel 会造成哪个不可绕过的安全缺口？**
   - 必须是不可绕过的安全原语，不是"为了方便"。

3. **新增的是通用治理原语，还是当前产品的便利字段？**
   - Kernel 只存储通用治理原语。产品专有字段请在外部 Harness 处理。

4. **换一套完全不同的外部系统后，它仍然成立吗？**
   - Kernel 不应该假设任何特定外部系统的存在。

5. **Kernel 是否开始理解新的业务事实？**
   - 一个修改让 Kernel 知道了新的业务事实，默认拒绝；除非能证明它是不可绕过的通用安全原语。
