/*
 * Copyright (c) 2023 Stalwart Labs Ltd.
 *
 * This file is part of Stalwart Mail Server.
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as
 * published by the Free Software Foundation, either version 3 of
 * the License, or (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 * GNU Affero General Public License for more details.
 * in the LICENSE file at the top-level directory of this distribution.
 * You should have received a copy of the GNU Affero General Public License
 * along with this program.  If not, see <http://www.gnu.org/licenses/>.
 *
 * You can be released from the requirements of the AGPLv3 license by
 * purchasing a commercial license. Please contact licensing@stalw.art
 * for more details.
*/

use mail_auth::{
    arc::ArcSet, dkim::Signature, dmarc::Policy, ArcOutput, AuthenticatedMessage,
    AuthenticationResults, DkimResult, DmarcResult, IprevResult, SpfResult,
};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;

use crate::config::{ArcSealer, DkimSigner};

pub mod auth;
pub mod data;
pub mod ehlo;
pub mod mail;
pub mod milter;
pub mod rcpt;
pub mod session;
pub mod spawn;
pub mod vrfy;

pub trait IsTls {
    fn is_tls(&self) -> bool;
    fn write_tls_header(&self, headers: &mut Vec<u8>);
    fn tls_version_and_cipher(&self) -> (&'static str, &'static str);
}

impl IsTls for TcpStream {
    fn is_tls(&self) -> bool {
        false
    }

    fn write_tls_header(&self, _headers: &mut Vec<u8>) {}

    fn tls_version_and_cipher(&self) -> (&'static str, &'static str) {
        ("", "")
    }
}

impl IsTls for TlsStream<TcpStream> {
    fn is_tls(&self) -> bool {
        true
    }

    fn tls_version_and_cipher(&self) -> (&'static str, &'static str) {
        let (_, conn) = self.get_ref();

        (
            match conn
                .protocol_version()
                .unwrap_or(rustls::ProtocolVersion::Unknown(0))
            {
                rustls::ProtocolVersion::SSLv2 => "SSLv2",
                rustls::ProtocolVersion::SSLv3 => "SSLv3",
                rustls::ProtocolVersion::TLSv1_0 => "TLSv1.0",
                rustls::ProtocolVersion::TLSv1_1 => "TLSv1.1",
                rustls::ProtocolVersion::TLSv1_2 => "TLSv1.2",
                rustls::ProtocolVersion::TLSv1_3 => "TLSv1.3",
                rustls::ProtocolVersion::DTLSv1_0 => "DTLSv1.0",
                rustls::ProtocolVersion::DTLSv1_2 => "DTLSv1.2",
                rustls::ProtocolVersion::DTLSv1_3 => "DTLSv1.3",
                _ => "unknown",
            },
            match conn.negotiated_cipher_suite() {
                Some(rustls::SupportedCipherSuite::Tls13(cs)) => {
                    cs.common.suite.as_str().unwrap_or("unknown")
                }
                Some(rustls::SupportedCipherSuite::Tls12(cs)) => {
                    cs.common.suite.as_str().unwrap_or("unknown")
                }
                None => "unknown",
            },
        )
    }

    fn write_tls_header(&self, headers: &mut Vec<u8>) {
        let (version, cipher) = self.tls_version_and_cipher();
        headers.extend_from_slice(b"(using ");
        headers.extend_from_slice(version.as_bytes());
        headers.extend_from_slice(b" with cipher ");
        headers.extend_from_slice(cipher.as_bytes());
        headers.extend_from_slice(b")\r\n\t");
    }
}

impl ArcSealer {
    pub fn seal<'x>(
        &self,
        message: &'x AuthenticatedMessage,
        results: &'x AuthenticationResults,
        arc_output: &'x ArcOutput,
    ) -> mail_auth::Result<ArcSet<'x>> {
        match self {
            ArcSealer::RsaSha256(sealer) => sealer.seal(message, results, arc_output),
            ArcSealer::Ed25519Sha256(sealer) => sealer.seal(message, results, arc_output),
        }
    }
}

impl DkimSigner {
    pub fn sign(&self, message: &[u8]) -> mail_auth::Result<Signature> {
        match self {
            DkimSigner::RsaSha256(signer) => signer.sign(message),
            DkimSigner::Ed25519Sha256(signer) => signer.sign(message),
        }
    }
    pub fn sign_chained(&self, message: &[&[u8]]) -> mail_auth::Result<Signature> {
        match self {
            DkimSigner::RsaSha256(signer) => signer.sign_chained(message.iter().copied()),
            DkimSigner::Ed25519Sha256(signer) => signer.sign_chained(message.iter().copied()),
        }
    }
}

pub trait AuthResult {
    fn as_str(&self) -> &'static str;
}

impl AuthResult for SpfResult {
    fn as_str(&self) -> &'static str {
        match self {
            SpfResult::Pass => "pass",
            SpfResult::Fail => "fail",
            SpfResult::SoftFail => "softfail",
            SpfResult::Neutral => "neutral",
            SpfResult::None => "none",
            SpfResult::TempError => "temperror",
            SpfResult::PermError => "permerror",
        }
    }
}

impl AuthResult for IprevResult {
    fn as_str(&self) -> &'static str {
        match self {
            IprevResult::Pass => "pass",
            IprevResult::Fail(_) => "fail",
            IprevResult::TempError(_) => "temperror",
            IprevResult::PermError(_) => "permerror",
            IprevResult::None => "none",
        }
    }
}

impl AuthResult for DkimResult {
    fn as_str(&self) -> &'static str {
        match self {
            DkimResult::Pass => "pass",
            DkimResult::None => "none",
            DkimResult::Neutral(_) => "neutral",
            DkimResult::Fail(_) => "fail",
            DkimResult::PermError(_) => "permerror",
            DkimResult::TempError(_) => "temperror",
        }
    }
}

impl AuthResult for DmarcResult {
    fn as_str(&self) -> &'static str {
        match self {
            DmarcResult::Pass => "pass",
            DmarcResult::Fail(_) => "fail",
            DmarcResult::TempError(_) => "temperror",
            DmarcResult::PermError(_) => "permerror",
            DmarcResult::None => "none",
        }
    }
}

impl AuthResult for Policy {
    fn as_str(&self) -> &'static str {
        match self {
            Policy::Reject => "reject",
            Policy::Quarantine => "quarantine",
            Policy::None | Policy::Unspecified => "none",
        }
    }
}
