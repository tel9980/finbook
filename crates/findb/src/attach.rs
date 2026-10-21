//! 凭证附件
//!
//! 会计的原始凭证要留存——发票扫描件、合同 PDF、银行回单。
//! 设计上做了个取巧：小于 256KB 的内联存进 SQLite（备份账套时一个文件带走），
//! 大文件落到账套同目录的 `.attachments/` 里只存相对路径。
//! 代价是大文件不会跟着账套文件一起复制，所以导出备份时得连目录一起打包。

use std::path::{Path, PathBuf};

use fincore::FinError;
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};

use crate::{Db, DbResult};

/// 内联存储的大小上限（字节）。超过就写磁盘。
pub const INLINE_LIMIT: usize = 256 * 1024;

/// 单文件最大大小限制（10MB），防止资源耗尽攻击
pub const MAX_FILE_SIZE: usize = 10 * 1024 * 1024;

/// 允许的文件扩展名白名单
pub const ALLOWED_EXTENSIONS: &[&str] = &[
    "pdf", "jpg", "jpeg", "png", "gif", "bmp", "tiff", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
    "txt", "csv", "zip", "rar", "7z", "xml", "json", "html", "htm",
];

/// 附件目录名（位于账套文件同级）
pub const DIR_NAME: &str = ".attachments";

/// 一条附件记录
#[derive(Clone, Debug)]
pub struct Attachment {
    pub id: i64,
    pub voucher_id: i64,
    pub name: String,
    /// 扩展名或 MIME 简写
    pub kind: String,
    /// 文件字节数
    pub size: i64,
    pub sha256: String,
    /// true = 数据在库里；false = 数据在 `.attachments/` 目录
    pub inline: bool,
    /// 相对 `.attachments/` 的路径
    pub path: Option<String>,
    pub added_by: String,
    pub added_at: String,
}

impl Attachment {
    /// 人类可读的大小
    pub fn size_text(&self) -> String {
        let b = self.size.max(0) as f64;
        if b < 1024.0 {
            format!("{} B", self.size)
        } else if b < 1024.0 * 1024.0 {
            format!("{:.1} KB", b / 1024.0)
        } else {
            format!("{:.1} MB", b / 1024.0 / 1024.0)
        }
    }

    /// 是否为图片（可以在界面里预览）
    pub fn is_image(&self) -> bool {
        matches!(
            self.kind.to_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp"
        )
    }
}

fn map(r: &rusqlite::Row) -> rusqlite::Result<Attachment> {
    Ok(Attachment {
        id: r.get(0)?,
        voucher_id: r.get(1)?,
        name: r.get(2)?,
        kind: r.get(3)?,
        size: r.get(4)?,
        sha256: r.get(5)?,
        inline: r.get::<_, i64>(6)? != 0,
        path: r.get(8)?,
        added_by: r.get(9)?,
        added_at: r.get(10)?,
    })
}

const COLS: &str = "id,voucher_id,name,kind,size,sha256,inline,data,path,added_by,added_at";

pub fn list(db: &Db, voucher_id: i64) -> DbResult<Vec<Attachment>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {COLS} FROM attachment WHERE voucher_id=?1 ORDER BY id"
    ))?;
    let rows = st
        .query_map(rusqlite::params![voucher_id], map)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn count(db: &Db, voucher_id: i64) -> DbResult<i64> {
    Ok(db.conn().query_row(
        "SELECT COUNT(*) FROM attachment WHERE voucher_id=?1",
        rusqlite::params![voucher_id],
        |r| r.get(0),
    )?)
}

/// 每张凭证的附件数量（列表批量展示用）
pub fn count_map(db: &Db, voucher_ids: &[i64]) -> DbResult<std::collections::HashMap<i64, i64>> {
    let mut m = std::collections::HashMap::new();
    if voucher_ids.is_empty() {
        return Ok(m);
    }
    let ph: Vec<String> = voucher_ids.iter().map(|_| "?".to_string()).collect();
    let sql = format!(
        "SELECT voucher_id, COUNT(*) FROM attachment WHERE voucher_id IN ({}) GROUP BY voucher_id",
        ph.join(",")
    );
    let mut st = db.conn().prepare(&sql)?;
    let mut rows = st.query(rusqlite::params_from_iter(voucher_ids.iter()))?;
    while let Some(r) = rows.next()? {
        m.insert(r.get(0)?, r.get(1)?);
    }
    Ok(m)
}

pub fn get(db: &Db, id: i64) -> DbResult<Option<Attachment>> {
    db.conn()
        .query_row(
            &format!("SELECT {COLS} FROM attachment WHERE id=?1"),
            rusqlite::params![id],
            map,
        )
        .optional()
        .map_err(Into::into)
}

/// 读取附件字节内容
pub fn read(db: &Db, id: i64) -> DbResult<Vec<u8>> {
    let a = get(db, id)?.ok_or_else(|| FinError::not_found("附件不存在"))?;
    if a.inline {
        let data: Vec<u8> = db.conn().query_row(
            "SELECT data FROM attachment WHERE id=?1",
            rusqlite::params![id],
            |r| r.get(0),
        )?;
        Ok(data)
    } else {
        let rel = a
            .path
            .clone()
            .ok_or_else(|| FinError::msg("附件记录缺少文件路径"))?;
        // 路径安全检查：防止路径穿越攻击
        if rel.contains("..") || rel.contains('/') || rel.contains('\\') {
            return Err(FinError::msg("无效的附件路径").into());
        }
        let p = dir_of(db).join(&rel);
        let bytes = std::fs::read(&p)
            .map_err(|e| FinError::io(format!("读取附件失败（{}）：{e}", p.display())))?;
        Ok(bytes)
    }
}

/// 账套同级的附件目录
pub fn dir_of(db: &Db) -> PathBuf {
    let dir = db.path().parent().unwrap_or_else(|| Path::new("."));
    dir.join(DIR_NAME)
}

/// 新增附件。返回 id。
pub fn add(db: &Db, voucher_id: i64, name: &str, data: &[u8], who: &str) -> DbResult<i64> {
    // 大小校验：防止超大文件导致内存/磁盘耗尽
    if data.len() > MAX_FILE_SIZE {
        return Err(FinError::msg(format!(
            "附件「{name}」超过大小限制（最大 {}MB）",
            MAX_FILE_SIZE / 1024 / 1024
        ))
        .into());
    }
    // 扩展名校验：白名单防止执行恶意文件上传
    let kind = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if !kind.is_empty() && !ALLOWED_EXTENSIONS.contains(&kind.as_str()) {
        return Err(FinError::msg(format!(
            "不允许的附件类型「.{kind}」，仅支持：{}",
            ALLOWED_EXTENSIONS.join(", ")
        ))
        .into());
    }
    let sha = sha256(data);
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let inline = data.len() <= INLINE_LIMIT;

    if inline {
        db.conn().execute(
            "INSERT INTO attachment(voucher_id,name,kind,size,sha256,inline,data,path,added_by,added_at)
             VALUES(?1,?2,?3,?4,?5,1,?6,NULL,?7,?8)",
            rusqlite::params![voucher_id, name, kind, data.len() as i64, sha, data, who, now],
        )?;
        return Ok(db.conn().last_insert_rowid());
    }

    // 大文件：落盘，文件名用 sha256 前 16 位避免重名
    let dir = dir_of(db);
    std::fs::create_dir_all(&dir)
        .map_err(|e| FinError::io(format!("创建附件目录失败（{}）：{e}", dir.display())))?;
    let rel = format!("{}.{kind}", &sha[..16.min(sha.len())]);
    let full = dir.join(&rel);
    std::fs::write(&full, data)
        .map_err(|e| FinError::io(format!("写入附件失败（{}）：{e}", full.display())))?;
    db.conn().execute(
        "INSERT INTO attachment(voucher_id,name,kind,size,sha256,inline,data,path,added_by,added_at)
         VALUES(?1,?2,?3,?4,?5,0,NULL,?6,?7,?8)",
        rusqlite::params![voucher_id, name, kind, data.len() as i64, sha, rel, who, now],
    )?;
    Ok(db.conn().last_insert_rowid())
}

/// 从磁盘文件导入附件
pub fn add_file(db: &Db, voucher_id: i64, src: &Path, who: &str) -> DbResult<i64> {
    let name = src
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "attachment".to_string());
    let data = std::fs::read(src)
        .map_err(|e| FinError::io(format!("读取文件失败（{}）：{e}", src.display())))?;
    add(db, voucher_id, &name, &data, who)
}

/// 删除附件（外存的会一并删文件）
pub fn delete(db: &Db, id: i64) -> DbResult<()> {
    let a = get(db, id)?;
    if let Some(a) = a {
        if !a.inline {
            if let Some(rel) = a.path {
                // 路径安全检查
                if rel.contains("..") || rel.contains('/') || rel.contains('\\') {
                    return Err(FinError::msg("无效的附件路径").into());
                }
                let p = dir_of(db).join(rel);
                // 删不掉也不算致命错误，只是留下孤儿文件
                let _ = std::fs::remove_file(p);
            }
        }
    }
    db.conn()
        .execute("DELETE FROM attachment WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

/// 凭证被删除时清理附件（由 vouchers::delete 调用）
pub fn delete_for_voucher(db: &Db, voucher_id: i64) -> DbResult<usize> {
    for a in list(db, voucher_id)? {
        let _ = delete(db, a.id);
    }
    db.conn()
        .execute(
            "DELETE FROM attachment WHERE voucher_id=?1",
            rusqlite::params![voucher_id],
        )
        .map_err(Into::into)
}

/// 改名
pub fn rename(db: &Db, id: i64, name: &str) -> DbResult<()> {
    db.conn().execute(
        "UPDATE attachment SET name=?2 WHERE id=?1",
        rusqlite::params![id, name],
    )?;
    Ok(())
}

/// 统计：内联占用 / 外存占用
pub struct AttachStat {
    pub count: i64,
    pub inline_bytes: i64,
    pub external_bytes: i64,
}

pub fn stat(db: &Db) -> DbResult<AttachStat> {
    // TEXT 列不参与聚合仍然安全，这里只做整数求和
    let (c, ib, eb): (i64, i64, i64) = db.conn().query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE WHEN inline=1 THEN size ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN inline=0 THEN size ELSE 0 END),0)
         FROM attachment",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(AttachStat {
        count: c,
        inline_bytes: ib,
        external_bytes: eb,
    })
}

/// 附件校验用 SHA-256（基于 sha2 crate，硬件可加速）；只是防止误删，不是防篡改。
fn sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    result.iter().map(|b| format!("{b:02x}")).collect()
}

/// 导出到临时目录的路径（"打开附件"时先把内容落地，再交给系统默认程序）
pub fn temp_export_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir().join("finbook_attachments");
    let stamp = chrono::Local::now().format("%Y%m%d%H%M%S");
    // 附件名可能来自上传方：只取文件名部分，替换分隔符/上跳字符，
    // 防止 "../x" 之类把落地文件写到临时目录之外。
    let safe: String = std::path::Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("attachment")
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == ':' {
                '_'
            } else {
                c
            }
        })
        .collect();
    p.push(format!("{stamp}_{safe}"));
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use fincore::voucher::{Entry, Voucher};

    /// 附件外键指向 voucher(id)，测试必须先建一张真凭证
    fn new_voucher(db: &Db) -> i64 {
        let p = fincore::Period::new(2026, 1).unwrap();
        let d = p.last_day();
        let no = crate::vouchers::next_no(db, p, "记").unwrap_or(1);
        let mut v = Voucher::new(p, d, "记", no);
        v.push_entry(Entry {
            debit: fincore::Money::parse("100").unwrap(),
            ..Entry::new(1, "1001", "测试")
        });
        v.push_entry(Entry {
            credit: fincore::Money::parse("100").unwrap(),
            ..Entry::new(2, "1001", "测试")
        });
        crate::vouchers::save(db, &mut v).unwrap()
    }

    #[test]
    fn inline_roundtrip() {
        let db = crate::tests::mem();
        let vid = new_voucher(&db);
        let data = b"hello attachment".to_vec();
        let id = add(&db, vid, "note.txt", &data, "tester").unwrap();
        let a = get(&db, id).unwrap().unwrap();
        assert!(a.inline);
        assert_eq!(a.name, "note.txt");
        assert_eq!(a.kind, "txt");
        assert_eq!(read(&db, id).unwrap(), data);
        assert_eq!(list(&db, vid).unwrap().len(), 1);
        assert_eq!(count(&db, vid).unwrap(), 1);
        delete(&db, id).unwrap();
        assert!(list(&db, vid).unwrap().is_empty());
    }

    #[test]
    fn sha_is_stable_and_unique() {
        assert_eq!(sha256(b"abc"), sha256(b"abc"));
        assert_ne!(sha256(b"abc"), sha256(b"abd"));
        assert_eq!(sha256(b"").len(), 64);
    }

    #[test]
    fn count_map_works() {
        let db = crate::tests::mem();
        let v1 = new_voucher(&db);
        let v2 = new_voucher(&db);
        add(&db, v1, "a.txt", b"a", "t").unwrap();
        add(&db, v1, "b.txt", b"b", "t").unwrap();
        add(&db, v2, "c.txt", b"c", "t").unwrap();
        let m = count_map(&db, &[v1, v2]).unwrap();
        assert_eq!(m.get(&v1), Some(&2));
        assert_eq!(m.get(&v2), Some(&1));
        delete_for_voucher(&db, v1).unwrap();
        assert_eq!(list(&db, v1).unwrap().len(), 0);
        assert_eq!(list(&db, v2).unwrap().len(), 1);
    }

    #[test]
    fn stat_sums_by_storage() {
        let db = crate::tests::mem();
        let vid = new_voucher(&db);
        add(&db, vid, "a.txt", b"12345", "t").unwrap();
        add(&db, vid, "b.txt", &vec![7u8; 1000], "t").unwrap();
        let s = stat(&db).unwrap();
        assert_eq!(s.count, 2);
        assert_eq!(s.inline_bytes, 1005);
        assert_eq!(s.external_bytes, 0);
    }
}
