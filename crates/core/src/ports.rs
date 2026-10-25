//! 仓储端口。

use crate::model::{Id, Instance, NewInstance};

/// 实例仓储。
pub trait Repository {
    /// 仓储错误。
    type Error;

    /// 列出全部实例。
    ///
    /// # Errors
    ///
    /// 目录无法读取时返回错误。
    fn list(&self) -> Result<Vec<Instance>, Self::Error>;

    /// 按标识加载实例。
    ///
    /// # Errors
    ///
    /// 实例不存在或文件损坏时返回错误。
    fn load(&self, id: &Id) -> Result<Instance, Self::Error>;

    /// 创建实例。
    ///
    /// # Errors
    ///
    /// 名称非法、配置非法或写入失败时返回错误。
    fn create(&self, spec: &NewInstance) -> Result<Instance, Self::Error>;

    /// 保存实例。
    ///
    /// # Errors
    ///
    /// 配置非法或写入失败时返回错误。
    fn save(&self, instance: &Instance) -> Result<(), Self::Error>;

    /// 删除实例。
    ///
    /// # Errors
    ///
    /// 实例不存在或目录无法删除时返回错误。
    fn delete(&self, id: &Id) -> Result<(), Self::Error>;

    /// 重命名实例标识。
    ///
    /// # Errors
    ///
    /// 源不存在、目标已存在、实例被锁定或移动失败时返回错误。
    fn rename(&self, from: &Id, to: &Id) -> Result<Instance, Self::Error>;
}
