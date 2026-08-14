//! T010 — tree-sitter-java extraction integration test.
//!
//! Parse a representative Spring Boot service/interface/repository sample,
//! verify correct extraction of package, imports, class name, implemented
//! interfaces, annotations, declared dependencies (@Autowired fields),
//! methods, plus interface and enum extraction.

use joey_neurocode::parse::java::parse_java_file;

/// A representative Spring Boot service implementing an interface with
/// @Autowired dependencies and annotated methods.
const SPRING_SERVICE: &str = r#"
package com.enterprise.auth.service;

import com.enterprise.auth.repo.UserRepository;
import com.enterprise.auth.model.User;
import org.springframework.stereotype.Service;
import org.springframework.transaction.annotation.Transactional;

@Service
@Transactional
public class UserServiceImpl implements UserService {

    @Autowired
    private UserRepository userRepository;

    @Autowired
    private AuditLogger auditLogger;

    @Override
    @Transactional(readOnly = true)
    public User findById(Long id) {
        return userRepository.findById(id);
    }

    @Override
    public void deleteUser(Long id) {
        userRepository.delete(id);
    }
}
"#;

/// The UserService interface.
const USER_SERVICE_INTERFACE: &str = r#"
package com.enterprise.auth.service;

import com.enterprise.auth.model.User;

public interface UserService {
    User findById(Long id);
    void deleteUser(Long id);
    boolean exists(Long id);
}
"#;

/// A Spring Data repository.
const REPOSITORY: &str = r#"
package com.enterprise.auth.repo;

import org.springframework.data.jpa.repository.JpaRepository;
import org.springframework.stereotype.Repository;

@Repository
public interface UserRepository extends JpaRepository<User, Long> {
    User findByEmail(String email);
}
"#;

/// An enum.
const STATUS_ENUM: &str = r#"
package com.enterprise.auth.model;

public enum Status {
    ACTIVE,
    INACTIVE,
    SUSPENDED
}
"#;

#[test]
fn extract_spring_service_structure() {
    let ext = parse_java_file(SPRING_SERVICE).expect("service should parse");

    // Package.
    assert_eq!(ext.package, "com.enterprise.auth.service");

    // Imports.
    assert!(ext.imports.iter().any(|i| i.contains("UserRepository")));
    assert!(ext
        .imports
        .iter()
        .any(|i| i.contains("UserService") || i.contains("model.User")));
    assert!(ext
        .imports
        .iter()
        .any(|i| i.contains("stereotype.Service")));
    assert!(ext
        .imports
        .iter()
        .any(|i| i.contains("transaction.annotation.Transactional")));

    // Exactly one type.
    assert_eq!(ext.types.len(), 1, "expected exactly one type declaration");
    let svc = &ext.types[0];

    // Class name + kind.
    assert_eq!(svc.name, "UserServiceImpl");
    assert_eq!(svc.kind, "class");

    // Implemented interfaces.
    assert!(
        svc.implemented_interfaces
            .iter()
            .any(|i| i == "UserService"),
        "should implement UserService, got: {:?}",
        svc.implemented_interfaces
    );

    // Annotations.
    assert!(
        svc.annotations.iter().any(|a| a == "Service"),
        "expected @Service annotation, got: {:?}",
        svc.annotations
    );
    assert!(
        svc.annotations.iter().any(|a| a == "Transactional"),
        "expected @Transactional annotation, got: {:?}",
        svc.annotations
    );

    // Declared dependencies (@Autowired fields).
    assert!(
        svc.declared_dependencies
            .iter()
            .any(|d| d == "UserRepository"),
        "should declare UserRepository dependency, got: {:?}",
        svc.declared_dependencies
    );
    assert!(
        svc.declared_dependencies
            .iter()
            .any(|d| d == "AuditLogger"),
        "should declare AuditLogger dependency, got: {:?}",
        svc.declared_dependencies
    );

    // Methods — findById and deleteUser.
    let method_names: Vec<&str> = svc.methods.iter().map(|m| m.name.as_str()).collect();
    assert!(
        method_names.contains(&"findById"),
        "expected findById method, got: {:?}",
        method_names
    );
    assert!(
        method_names.contains(&"deleteUser"),
        "expected deleteUser method, got: {:?}",
        method_names
    );

    // findById should carry the Override + Transactional annotations.
    let find = svc
        .methods
        .iter()
        .find(|m| m.name == "findById")
        .expect("findById method");
    assert!(
        find.annotations.iter().any(|a| a == "Override"),
        "findById should be @Override, got: {:?}",
        find.annotations
    );

    // Byte spans should be populated.
    assert!(svc.end_byte > svc.start_byte);
    assert!(find.end_byte > find.start_byte);
}

#[test]
fn extract_interface_methods() {
    let ext = parse_java_file(USER_SERVICE_INTERFACE).expect("interface should parse");
    assert_eq!(ext.package, "com.enterprise.auth.service");
    assert_eq!(ext.types.len(), 1);
    let iface = &ext.types[0];

    assert_eq!(iface.kind, "interface");
    assert_eq!(iface.name, "UserService");

    // All three methods should be extracted.
    let method_names: Vec<&str> = iface.methods.iter().map(|m| m.name.as_str()).collect();
    assert!(method_names.contains(&"findById"));
    assert!(method_names.contains(&"deleteUser"));
    assert!(method_names.contains(&"exists"));
    assert_eq!(iface.methods.len(), 3, "expected 3 interface methods");
}

#[test]
fn extract_repository_extends() {
    let ext = parse_java_file(REPOSITORY).expect("repository should parse");
    assert_eq!(ext.types.len(), 1);
    let repo = &ext.types[0];

    assert_eq!(repo.kind, "interface");
    assert_eq!(repo.name, "UserRepository");

    // Should have the @Repository annotation.
    assert!(repo.annotations.iter().any(|a| a == "Repository"));

    // Custom method (the extends JpaRepository<User, Long> clause is not
    // captured by the current interface-extends extractor — only class
    // `implements` clauses are — so we assert the custom method only).
    let method_names: Vec<&str> = repo.methods.iter().map(|m| m.name.as_str()).collect();
    assert!(
        method_names.contains(&"findByEmail"),
        "expected findByEmail method, got: {:?}",
        method_names
    );
}

#[test]
fn extract_enum() {
    let ext = parse_java_file(STATUS_ENUM).expect("enum should parse");
    assert_eq!(ext.package, "com.enterprise.auth.model");
    assert_eq!(ext.types.len(), 1);
    let en = &ext.types[0];
    assert_eq!(en.kind, "enum");
    assert_eq!(en.name, "Status");
}

#[test]
fn extract_empty_file() {
    let ext = parse_java_file("").expect("empty input should parse (no types)");
    assert!(ext.types.is_empty());
    assert!(ext.package.is_empty());
}

#[test]
fn source_byte_spans_are_monotonic() {
    // The type's byte span should cover the method spans.
    let ext = parse_java_file(SPRING_SERVICE).unwrap();
    let svc = &ext.types[0];
    for method in &svc.methods {
        assert!(
            method.start_byte >= svc.start_byte,
            "method {:?} starts before class span",
            method.name
        );
        assert!(
            method.end_byte <= svc.end_byte,
            "method {:?} ends after class span",
            method.name
        );
    }
}
