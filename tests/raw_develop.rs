use fastcull::xmp::{
    self, BasicAdjustments, BasicControl, Updates, WhiteBalance, WhiteBalanceSettings,
};
use std::{fs, path::Path};

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const CRS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";

fn attributes(fields: &str) -> String {
    format!(r#"<rdf:Description xmlns:rdf="{RDF}" xmlns:crs="{CRS}" {fields}/>"#)
}

fn read_packet(text: &str) -> anyhow::Result<xmp::Values> {
    let dir = tempfile::tempdir()?;
    let raw = dir.path().join("image.ARW");
    fs::write(raw.with_extension("xmp"), text)?;
    xmp::read(&raw)
}

#[test]
fn custom_raw_edits_roundtrip_without_rewriting_adobe_dxo_or_look_settings() {
    let dir = tempfile::tempdir().unwrap();
    for extension in ["ARW", "NEF"] {
        let raw = dir.path().join(format!("image.{extension}"));
        // An actual raw container is not needed for a sidecar-only edit.
        fs::write(&raw, b"original raw bytes").unwrap();
        let original = format!(
            r#"<?xpacket begin=""?><rdf:Description xmlns:rdf="{RDF}" xmlns:camera="{CRS}" xmlns:dxo="urn:dxo:test" camera:WhiteBalance="Auto" camera:Temperature="4900" camera:Tint="+17" camera:Exposure2012="0.250000" camera:HasSettings="True" camera:ProcessVersion="15.4" camera:CameraProfile="Camera Standard" camera:CameraProfileDigest="012345" camera:Contrast2012="23" dxo:Settings="untouched"><camera:Look><rdf:Description camera:Name="Adobe Color" camera:Amount="1.000000"/></camera:Look><dxo:Opaque>keep &amp; preserve</dxo:Opaque></rdf:Description><?xpacket end="w"?>"#
        );
        fs::write(raw.with_extension("xmp"), &original).unwrap();
        let edit = WhiteBalance::Custom {
            temperature: 5_200,
            tint: -9,
        };
        assert!(
            xmp::write(
                &raw,
                &Updates {
                    white_balance: Some(edit),
                    exposure: Some(750_000),
                    ..Default::default()
                }
            )
            .unwrap()
        );
        let actual = xmp::packet(&raw).unwrap().unwrap();
        let expected = original
            .replace("WhiteBalance=\"Auto\"", "WhiteBalance=\"Custom\"")
            .replace("Temperature=\"4900\"", "Temperature=\"5200\"")
            .replace("Tint=\"+17\"", "Tint=\"-9\"")
            .replace("Exposure2012=\"0.250000\"", "Exposure2012=\"0.750000\"");
        assert_eq!(actual, expected);
        let values = xmp::read(&raw).unwrap();
        assert_eq!(values.white_balance.unwrap().edit(), Some(edit));
        assert_eq!(values.camera_profile.as_deref(), Some("Camera Standard"));
        assert_eq!(values.process_version.as_deref(), Some("15.4"));
        assert_eq!(values.exposure, Some(750_000));
        assert_eq!(fs::read(&raw).unwrap(), b"original raw bytes");
    }
}

#[test]
fn as_shot_can_omit_numbers_and_reset_preserves_existing_sensor_derived_values() {
    let values = read_packet(&attributes(r#"crs:WhiteBalance="As Shot""#)).unwrap();
    assert_eq!(
        values.white_balance.unwrap(),
        WhiteBalanceSettings::from(WhiteBalance::AsShot)
    );
    assert_eq!(read_packet(&attributes("")).unwrap().white_balance, None);
    let original = attributes(
        r#"crs:WhiteBalance="Custom" crs:Temperature="4850" crs:Tint="+17" crs:HasSettings="True""#,
    );
    let patched = xmp::patch(
        &original,
        &Updates {
            white_balance: Some(WhiteBalance::AsShot),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(patched, original.replace("Custom", "As Shot"));
    let values = read_packet(&patched).unwrap().white_balance.unwrap();
    assert_eq!(values.edit(), Some(WhiteBalance::AsShot));
    assert_eq!(values.temperature, Some(4_850));
    assert_eq!(values.tint, Some(17));
}

#[test]
fn preset_unknown_and_incomplete_values_are_visible_without_assuming_a_custom_balance() {
    for mode in ["Auto", "Daylight", "Vendor Mode"] {
        let text = attributes(&format!(
            r#"crs:WhiteBalance="{mode}" crs:Temperature="5100" crs:Tint="+7""#
        ));
        let wb = read_packet(&text).unwrap().white_balance.unwrap();
        assert_eq!(wb.mode.as_deref(), Some(mode));
        assert_eq!(wb.temperature, Some(5_100));
        assert_eq!(wb.tint, Some(7));
        assert_eq!(wb.edit(), None);
        let patch = xmp::patch(
            &text,
            &Updates {
                exposure: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(patch.contains(&format!(r#"crs:WhiteBalance="{mode}""#)));
    }
    for fields in [
        r#"crs:WhiteBalance="Custom""#,
        r#"crs:WhiteBalance="Custom" crs:Temperature="5100""#,
        r#"crs:Temperature="5100" crs:Tint="+7""#,
        r#"crs:WhiteBalance="Custom" crs:Temperature="1500" crs:Tint="175""#,
    ] {
        assert_eq!(
            read_packet(&attributes(fields))
                .unwrap()
                .white_balance
                .unwrap()
                .edit(),
            None
        );
    }
}

#[test]
fn element_fields_resolve_namespace_and_expose_profile_reference_without_changing_it() {
    let text = format!(
        r#"<r:Description xmlns:r="{RDF}" xmlns:c="{CRS}" xmlns:other="urn:other"><other:WhiteBalance>Custom</other:WhiteBalance><c:WhiteBalance>Custom</c:WhiteBalance><c:Temperature>5000</c:Temperature><c:Tint>+17</c:Tint><c:CameraProfile>Camera &amp; Lens</c:CameraProfile><c:ProcessVersion>15.4</c:ProcessVersion></r:Description>"#
    );
    let values = read_packet(&text).unwrap();
    assert_eq!(values.camera_profile.as_deref(), Some("Camera & Lens"));
    assert_eq!(values.process_version.as_deref(), Some("15.4"));
    assert_eq!(
        values.white_balance.unwrap().edit(),
        Some(WhiteBalance::Custom {
            temperature: 5_000,
            tint: 17
        })
    );
    let actual = xmp::patch(
        &text,
        &Updates {
            white_balance: Some(WhiteBalance::Custom {
                temperature: 6_000,
                tint: -3,
            }),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(actual.contains("<c:Temperature>6000</c:Temperature><c:Tint>-3</c:Tint>"));
    assert!(actual.contains("<other:WhiteBalance>Custom</other:WhiteBalance>"));
    assert!(actual.contains("<c:CameraProfile>Camera &amp; Lens</c:CameraProfile>"));
    assert!(actual.contains("<c:ProcessVersion>15.4</c:ProcessVersion>"));
}

#[test]
fn malformed_or_ambiguous_white_balance_never_becomes_a_default_baseline() {
    for fields in [
        r#"crs:WhiteBalance="""#,
        r#"crs:Temperature="0""#,
        r#"crs:Temperature="-1""#,
        r#"crs:Temperature="NaN""#,
        r#"crs:Temperature="Infinity""#,
        r#"crs:Temperature="5100.5""#,
        r#"crs:Temperature="4294967296""#,
        r#"crs:Tint="+7.5""#,
        r#"crs:Tint="NaN""#,
        r#"crs:Tint="2147483648""#,
    ] {
        assert!(read_packet(&attributes(fields)).is_err(), "{fields}");
    }
    let conflicting = format!(
        r#"<rdf:Description xmlns:rdf="{RDF}" xmlns:crs="{CRS}" crs:WhiteBalance="Custom" crs:Temperature="5000" crs:Tint="0"><crs:Temperature>6000</crs:Temperature></rdf:Description>"#
    );
    assert!(
        read_packet(&conflicting)
            .unwrap_err()
            .to_string()
            .contains("conflicting crs:Temperature")
    );
}

#[test]
fn invalid_atomic_edit_fails_before_creating_a_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("image.NEF");
    for edit in [
        WhiteBalance::Custom {
            temperature: 1_999,
            tint: 0,
        },
        WhiteBalance::Custom {
            temperature: 50_001,
            tint: 0,
        },
        WhiteBalance::Custom {
            temperature: 5_000,
            tint: -151,
        },
        WhiteBalance::Custom {
            temperature: 5_000,
            tint: 151,
        },
    ] {
        assert!(
            xmp::write(
                &raw,
                &Updates {
                    white_balance: Some(edit),
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert!(!raw.with_extension("xmp").exists());
    }
    let edit = Updates {
        white_balance: Some(WhiteBalance::AsShot),
        ..Default::default()
    };
    assert!(!edit.empty() && !edit.zero());
    assert!(xmp::write(&raw, &edit).unwrap());
    assert_eq!(
        xmp::read(&raw).unwrap().white_balance.unwrap().edit(),
        Some(WhiteBalance::AsShot)
    );
}

#[test]
fn new_render_draft_marks_develop_settings_without_selecting_profile_or_process_version() {
    let draft = xmp::patch(
        &xmp::template(),
        &Updates {
            white_balance: Some(WhiteBalance::Custom {
                temperature: 5_200,
                tint: 17,
            }),
            exposure: Some(500_000),
            crop: Some(fastcull::crop::Crop::default()),
            ..Default::default()
        },
    )
    .unwrap();
    let values = read_packet(&draft).unwrap();
    assert_eq!(values.exposure, Some(500_000));
    assert_eq!(
        values.white_balance.unwrap().edit(),
        Some(WhiteBalance::Custom {
            temperature: 5_200,
            tint: 17
        })
    );
    assert!(values.camera_profile.is_none() && values.process_version.is_none());
    assert!(draft.contains("crs:HasSettings=\"True\""));
    assert_eq!(draft.matches("crs:HasSettings=").count(), 1);
}

fn embedded_dng(path: &Path, packet: &str) {
    // Minimal little-endian TIFF: IFD0 tag 700 (XMP), followed by its UTF-8 packet.
    let mut bytes = b"II\x2a\0\x08\0\0\0".to_vec();
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(700u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend((packet.len() as u32).to_le_bytes());
    bytes.extend(26u32.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(packet.as_bytes());
    fs::write(path, bytes).unwrap();
}

#[test]
fn renderer_packet_uses_dng_embedded_xmp_then_authoritative_sidecar_without_writing_raw() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("linear.DNG");
    let embedded = attributes(
        r#"crs:WhiteBalance="As Shot" crs:CameraProfile="Embedded" crs:ProcessVersion="15.4""#,
    );
    embedded_dng(&raw, &embedded);
    let original = fs::read(&raw).unwrap();
    assert_eq!(
        xmp::packet(&raw).unwrap().as_deref(),
        Some(embedded.as_str())
    );
    assert_eq!(
        xmp::read(&raw).unwrap().camera_profile.as_deref(),
        Some("Embedded")
    );
    let sidecar = attributes(
        r#"crs:WhiteBalance="Custom" crs:Temperature="5100" crs:Tint="3" crs:CameraProfile="Adobe Color""#,
    );
    fs::write(raw.with_extension("xmp"), &sidecar).unwrap();
    assert_eq!(
        xmp::packet(&raw).unwrap().as_deref(),
        Some(sidecar.as_str())
    );
    assert_eq!(
        xmp::read(&raw).unwrap().camera_profile.as_deref(),
        Some("Adobe Color")
    );
    assert_eq!(fs::read(&raw).unwrap(), original);
    for extension in ["ARW", "NEF"] {
        assert!(
            xmp::packet(&dir.path().join(format!("missing.{extension}")))
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn merging_catalog_develop_keeps_new_source_decisions_and_non_adobe_metadata() {
    let source = format!(
        r#"<?xpacket begin=""?><x:xmpmeta xmlns:x="adobe:ns:meta/"><r:RDF xmlns:r="{RDF}"><r:Description r:about="" xmlns:c="{CRS}" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:sr="urn:zrtc:syncratings:1/" xmlns:v="urn:vendor" xmp:Rating="5" xmp:Label="Red" sr:Flag="pick" c:Exposure2012="-1" v:Opaque="keep &amp; exact"><v:Blob><![CDATA[unmodified <payload>]]></v:Blob><!-- keep source comment --><c:OldMask><r:Bag><r:li c:LegacyValue="old"/></r:Bag></c:OldMask></r:Description></r:RDF></x:xmpmeta><?xpacket end="w"?>"#
    );
    let adobe = format!(
        r#"<r:RDF xmlns:r="{RDF}" xmlns:c="{CRS}" xmlns:xmp="http://ns.adobe.com/xap/1.0/"><r:Description r:about="" xmp:Rating="1" xmp:Label="Blue" c:Exposure2012="0.750000" c:ProcessVersion="15.4" c:CameraProfile="Camera Standard"><c:NewUnknown><r:Seq><r:li c:Resource="image.acr" c:Opaque="future"/></r:Seq></c:NewUnknown></r:Description></r:RDF>"#
    );
    let merged = xmp::merge_develop(&source, &adobe).unwrap();
    let values = read_packet(&merged).unwrap();
    assert_eq!(values.rating, Some(5));
    assert_eq!(values.label.as_deref(), Some("Red"));
    assert_eq!(values.flag.as_deref(), Some("pick"));
    assert_eq!(values.exposure, Some(750_000));
    assert_eq!(values.camera_profile.as_deref(), Some("Camera Standard"));
    assert_eq!(values.process_version.as_deref(), Some("15.4"));
    assert!(merged.contains(r#"v:Opaque="keep &amp; exact"><v:Blob><![CDATA[unmodified <payload>]]></v:Blob><!-- keep source comment -->"#));
    assert!(merged.contains(r#"<c:NewUnknown><r:Seq><r:li c:Resource="image.acr" c:Opaque="future"/></r:Seq></c:NewUnknown>"#));
    assert!(!merged.contains("OldMask") && !merged.contains("LegacyValue"));
    assert!(!merged.contains(r#"xmp:Rating="1""#) && !merged.contains(r#"xmp:Label="Blue""#));
}

fn element_namespace(packet: &str, local: &[u8]) -> Option<String> {
    use quick_xml::{NsReader, events::Event, name::ResolveResult};
    let mut reader = NsReader::from_str(packet);
    loop {
        let (namespace, event) = reader.read_resolved_event().unwrap();
        match event {
            Event::Start(element) | Event::Empty(element)
                if element.local_name().as_ref() == local =>
            {
                return match namespace {
                    ResolveResult::Bound(uri) => {
                        Some(String::from_utf8(uri.as_ref().to_vec()).unwrap())
                    }
                    ResolveResult::Unbound => Some(String::new()),
                    ResolveResult::Unknown(prefix) => panic!("unbound prefix {prefix:?}"),
                };
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

#[test]
fn merging_preserves_nested_namespace_context_with_aliases_defaults_and_prefix_collisions() {
    let source = format!(
        r#"<r:RDF xmlns:r="{RDF}" xmlns="urn:source-default" xmlns:c="urn:source-vendor"><r:Description r:about="" c:Metadata="untouched"><c:Payload>keep</c:Payload></r:Description></r:RDF>"#
    );
    let adobe = format!(
        r#"<r:RDF xmlns:r="{RDF}" xmlns:c="{CRS}" xmlns="urn:adobe-default" xmlns:asset="urn:assets" xmlns:fcRdf="urn:other"><r:Description r:about="" c:Future="exact"><c:Look><r:Description><asset:Table r:resource="table.bin"/><NativePayload><asset:Entry/></NativePayload><c:Mask><r:Seq><r:li xmlns:asset="urn:inner-assets"><asset:InnerPayload/></r:li></r:Seq></c:Mask></r:Description></c:Look></r:Description></r:RDF>"#
    );
    let merged = xmp::merge_develop(&source, &adobe).unwrap();
    assert!(merged.contains(r#"c:Metadata="untouched"><c:Payload>keep</c:Payload>"#));
    assert!(merged.contains(r#"c:Future="exact""#));
    assert_eq!(
        element_namespace(&merged, b"Payload").as_deref(),
        Some("urn:source-vendor")
    );
    assert_eq!(element_namespace(&merged, b"Look").as_deref(), Some(CRS));
    assert_eq!(
        element_namespace(&merged, b"NativePayload").as_deref(),
        Some("urn:adobe-default")
    );
    assert_eq!(
        element_namespace(&merged, b"Table").as_deref(),
        Some("urn:assets")
    );
    assert_eq!(
        element_namespace(&merged, b"InnerPayload").as_deref(),
        Some("urn:inner-assets")
    );
    let no_default = format!(
        r#"<r:RDF xmlns:r="{RDF}" xmlns:c="{CRS}"><r:Description><c:Look><UnqualifiedPayload/></c:Look></r:Description></r:RDF>"#
    );
    let merged = xmp::merge_develop(&source, &no_default).unwrap();
    assert_eq!(
        element_namespace(&merged, b"UnqualifiedPayload").as_deref(),
        Some("")
    );
}

#[test]
fn merged_default_namespace_properties_remain_readable_and_editable() {
    let source = format!(
        r#"<RDF xmlns="{RDF}" xmlns:xmp="http://ns.adobe.com/xap/1.0/"><Description xmp:Rating="5"/></RDF>"#
    );
    let adobe = format!(
        r#"<r:RDF xmlns:r="{RDF}" xmlns="{CRS}"><r:Description><Exposure2012>1.25</Exposure2012><ProcessVersion>15.4</ProcessVersion></r:Description></r:RDF>"#
    );
    let merged = xmp::merge_develop(&source, &adobe).unwrap();
    let values = read_packet(&merged).unwrap();
    assert_eq!(values.rating, Some(5));
    assert_eq!(values.exposure, Some(1_250_000));
    let edited = xmp::patch(
        &merged,
        &Updates {
            exposure: Some(2_000_000),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(edited.contains("<Exposure2012>2.000000</Exposure2012>"));
    assert_eq!(read_packet(&edited).unwrap().exposure, Some(2_000_000));
    // The unprefixed attribute has no namespace even inside a default-CRS scope.
    let unqualified =
        format!(r#"<r:Description xmlns:r="{RDF}" xmlns="{CRS}" Exposure2012="4.0"/>"#);
    assert_eq!(read_packet(&unqualified).unwrap().exposure, None);
}

#[test]
fn merging_replaces_all_develop_descriptions_and_supports_legacy_bare_description_packets() {
    let source = format!(
        r#"<r:RDF xmlns:r="{RDF}" xmlns:c="{CRS}" xmlns:v="urn:vendor"><r:Description r:about="" c:UnknownOld="delete" v:Metadata="one"/><r:Description r:about="" c:ProcessVersion="5.7"><v:Content>two</v:Content></r:Description></r:RDF>"#
    );
    let adobe = format!(
        r#"<r:RDF xmlns:r="{RDF}" xmlns:camera="{CRS}"><r:Description r:about="" camera:ProcessVersion="15.4"/><r:Description r:about="" camera:Exposure2012="1.000000"/></r:RDF>"#
    );
    let merged = xmp::merge_develop(&source, &adobe).unwrap();
    assert!(!merged.contains("UnknownOld") && !merged.contains("5.7"));
    assert!(
        merged.contains(r#"v:Metadata="one""#) && merged.contains("<v:Content>two</v:Content>")
    );
    assert_eq!(read_packet(&merged).unwrap().exposure, Some(1_000_000));
    let bare = attributes(r#"crs:Exposure2012="0""#);
    let merged = xmp::merge_develop(&bare, &adobe).unwrap();
    assert_eq!(
        read_packet(&merged).unwrap().process_version.as_deref(),
        Some("15.4")
    );
    assert_eq!(read_packet(&merged).unwrap().exposure, Some(1_000_000));
    let empty_rdf = format!(r#"<r:RDF xmlns:r="{RDF}"/>"#);
    let merged = xmp::merge_develop(&empty_rdf, &adobe).unwrap();
    assert_eq!(read_packet(&merged).unwrap().exposure, Some(1_000_000));
}

#[test]
fn merging_an_empty_develop_baseline_removes_old_settings_and_keeps_other_bytes_exact() {
    let source = format!(
        r#"<r:RDF xmlns:r="{RDF}" xmlns:c="{CRS}" xmlns:v="urn:vendor"><r:Description  c:Old = 'value' v:Metadata="raw &amp; exact"><c:OldStructure><r:Seq/></c:OldStructure><!--keep--><v:Tag>unchanged</v:Tag></r:Description></r:RDF>"#
    );
    let adobe = attributes("");
    let expected = source
        .replace("c:Old = 'value'", "")
        .replace("<c:OldStructure><r:Seq/></c:OldStructure>", "");
    assert_eq!(xmp::merge_develop(&source, &adobe).unwrap(), expected);
    let untouched = format!(r#"<r:RDF xmlns:r="{RDF}"><r:Description/></r:RDF>"#);
    assert_eq!(xmp::merge_develop(&untouched, &adobe).unwrap(), untouched);
}

#[test]
fn merging_rejects_ambiguous_or_invalid_namespace_and_subject_data() {
    let source = xmp::template();
    for adobe in [
        format!(
            r#"<r:RDF xmlns:r="{RDF}" xmlns:c="{CRS}"><r:Description c:Exposure2012="1"><c:Exposure2012>2</c:Exposure2012></r:Description></r:RDF>"#
        ),
        format!(
            r#"<r:RDF xmlns:r="{RDF}" xmlns:c="{CRS}"><r:Description r:about="another-image" c:Exposure2012="1"/></r:RDF>"#
        ),
        format!(r#"<r:RDF xmlns:r="{RDF}"><r:Description><missing:Look/></r:Description></r:RDF>"#),
        format!(
            r#"<!DOCTYPE r:RDF [<!ENTITY f "value">]><r:RDF xmlns:r="{RDF}"><r:Description/></r:RDF>"#
        ),
        format!(
            r#"<r:RDF xmlns:r="{RDF}" xmlns:a="{CRS}" xmlns:b="{CRS}"><r:Description a:Exposure2012="1" b:Exposure2012="2"/></r:RDF>"#
        ),
        format!(r#"<r:RDF xmlns:r="{RDF}" xml:base="../other/"><r:Description/></r:RDF>"#),
        format!(r#"<r:RDF xmlns:r="{RDF}"><r:Description><r:Bag></r:Description></r:RDF>"#),
        format!(r#"<r:RDF xmlns:r="{RDF}"><r:Description r:nodeID="blank"/></r:RDF>"#),
        format!(
            r#"<r:RDF xmlns:r="{RDF}" xmlns:c="{CRS}"><r:Description c:Value="&#0;"/></r:RDF>"#
        ),
        format!(
            r#"<r:RDF xmlns:r="{RDF}" xmlns:c="{CRS}"><r:Description><c:Value>&#0;</c:Value></r:Description></r:RDF>"#
        ),
    ] {
        assert!(xmp::merge_develop(&source, &adobe).is_err(), "{adobe}");
    }
}

#[test]
fn legacy_exposure_reads_and_writes_its_active_field_without_migrating_process_version() {
    for version in ["5.0", "5.7"] {
        let source = attributes(&format!(
            r#"crs:ProcessVersion="{version}" crs:Exposure="+1.25" crs:Exposure2012="4.5" crs:AutoExposure="True" crs:Brightness="50" crs:Contrast="25" crs:CameraProfile="Camera Standard""#
        ));
        assert_eq!(read_packet(&source).unwrap().exposure, Some(1_250_000));
        assert_eq!(
            xmp::exposure_range(&source).unwrap(),
            -4_000_000..=4_000_000
        );
        let edited = xmp::patch(
            &source,
            &Updates {
                exposure: Some(1_500_000),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(edited.contains(r#"crs:Exposure="1.500000""#));
        assert!(
            edited.contains(r#"crs:Exposure2012="4.5""#),
            "inactive modern setting must not be rewritten"
        );
        assert!(edited.contains(r#"crs:AutoExposure="False""#));
        assert!(edited.contains(
            r#"crs:Brightness="50" crs:Contrast="25" crs:CameraProfile="Camera Standard""#
        ));
        let values = read_packet(&edited).unwrap();
        assert_eq!(values.exposure, Some(1_500_000));
        assert_eq!(values.process_version.as_deref(), Some(version));
        let wb_only = xmp::patch(
            &source,
            &Updates {
                white_balance: Some(WhiteBalance::AsShot),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(wb_only.contains(r#"crs:AutoExposure="True""#));
    }
    let element = format!(
        r#"<r:Description xmlns:r="{RDF}" xmlns:c="{CRS}"><c:ProcessVersion>5.7</c:ProcessVersion><c:Exposure>0.25</c:Exposure><c:AutoExposure>True</c:AutoExposure></r:Description>"#
    );
    let edited = xmp::patch(
        &element,
        &Updates {
            exposure: Some(-500_000),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(edited.contains("<c:Exposure>-0.500000</c:Exposure>"));
    assert!(edited.contains("<c:AutoExposure>False</c:AutoExposure>"));
    assert!(!edited.contains("Exposure2012"));
}

#[test]
fn modern_process_uses_exposure2012_and_keeps_inactive_legacy_values() {
    for version in ["6.6", "6.7", "10.0", "11.0", "15.4"] {
        let source = attributes(&format!(
            r#"crs:ProcessVersion="{version}" crs:Exposure="-3.5" crs:Exposure2012="1.25" crs:AutoExposure="True""#
        ));
        assert_eq!(read_packet(&source).unwrap().exposure, Some(1_250_000));
        assert_eq!(
            xmp::exposure_range(&source).unwrap(),
            -5_000_000..=5_000_000
        );
        let edited = xmp::patch(
            &source,
            &Updates {
                exposure: Some(2_000_000),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(edited.contains(r#"crs:Exposure2012="2.000000""#));
        assert!(edited.contains(r#"crs:Exposure="-3.5""#));
        assert!(edited.contains(r#"crs:AutoExposure="True""#));
        assert_eq!(
            read_packet(&edited).unwrap().process_version.as_deref(),
            Some(version)
        );
    }
}

#[test]
fn unversioned_exposure_inference_does_not_arbitrarily_choose_between_two_fields() {
    let legacy = attributes(r#"crs:Exposure="-0.75""#);
    assert_eq!(read_packet(&legacy).unwrap().exposure, Some(-750_000));
    assert_eq!(
        xmp::exposure_range(&legacy).unwrap(),
        -4_000_000..=4_000_000
    );
    let modern = attributes(r#"crs:Exposure2012="0.75""#);
    assert_eq!(read_packet(&modern).unwrap().exposure, Some(750_000));
    assert_eq!(
        xmp::exposure_range(&modern).unwrap(),
        -5_000_000..=5_000_000
    );
    let ambiguous = attributes(r#"crs:Exposure="-0.75" crs:Exposure2012="0.75""#);
    assert_eq!(read_packet(&ambiguous).unwrap().exposure, None);
    assert!(xmp::exposure_range(&ambiguous).is_err());
    assert!(
        xmp::patch(
            &ambiguous,
            &Updates {
                exposure: Some(0),
                ..Default::default()
            }
        )
        .is_err()
    );
    let rating = xmp::patch(
        &ambiguous,
        &Updates {
            rating: Some(5),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(rating.contains(r#"crs:Exposure="-0.75" crs:Exposure2012="0.75""#));
}

#[test]
fn unknown_process_or_legacy_out_of_range_exposure_never_writes_or_changes_the_process() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("legacy.ARW");
    for version in ["4.0", "5.9", "6.0", "6.65", "unknown", "NaN"] {
        let source = attributes(&format!(
            r#"crs:ProcessVersion="{version}" crs:Exposure="0.5" crs:Exposure2012="1.5""#
        ));
        fs::write(raw.with_extension("xmp"), &source).unwrap();
        assert_eq!(xmp::read(&raw).unwrap().exposure, None);
        assert!(xmp::exposure_range(&source).is_err());
        assert!(
            xmp::write(
                &raw,
                &Updates {
                    exposure: Some(500_000),
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert_eq!(
            fs::read_to_string(raw.with_extension("xmp")).unwrap(),
            source
        );
        let wb_only = xmp::patch(
            &source,
            &Updates {
                white_balance: Some(WhiteBalance::AsShot),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            read_packet(&wb_only).unwrap().process_version.as_deref(),
            Some(version)
        );
    }
    let legacy = attributes(r#"crs:ProcessVersion="5.7" crs:Exposure="0""#);
    for value in [-4_000_001, 4_000_001] {
        assert!(
            xmp::patch(
                &legacy,
                &Updates {
                    exposure: Some(value),
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    for value in [-4_000_000, 4_000_000] {
        let edited = xmp::patch(
            &legacy,
            &Updates {
                exposure: Some(value),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(read_packet(&edited).unwrap().exposure, Some(value));
    }
}

#[test]
fn seven_basic_values_roundtrip_and_partial_reset_preserves_other_settings_exactly() {
    let original = attributes(
        r#"crs:ProcessVersion="15.4" crs:HasSettings="True" crs:Contrast2012="+21" crs:Highlights2012="-30" crs:Shadows2012="40" crs:Whites2012="+12" crs:Blacks2012="-15" crs:Vibrance="20" crs:Saturation="-8" crs:UnknownFuture="preserved""#,
    );
    let values = xmp::parse_values(&original).unwrap();
    assert_eq!(
        values.basic,
        BasicAdjustments {
            contrast: Some(21),
            highlights: Some(-30),
            shadows: Some(40),
            whites: Some(12),
            blacks: Some(-15),
            vibrance: Some(20),
            saturation: Some(-8),
        }
    );
    let updates = Updates {
        basic: BasicAdjustments {
            contrast: Some(0),
            highlights: Some(-15),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(!updates.empty() && !updates.zero());
    let edited = xmp::patch(&original, &updates).unwrap();
    assert_eq!(
        edited,
        original
            .replace("Contrast2012=\"+21\"", "Contrast2012=\"0\"")
            .replace("Highlights2012=\"-30\"", "Highlights2012=\"-15\"")
    );
    let values = xmp::parse_values(&edited).unwrap();
    assert_eq!(values.basic.contrast, Some(0));
    assert_eq!(values.basic.highlights, Some(-15));
    assert_eq!(values.basic.saturation, Some(-8));
}

#[test]
fn basic_element_fields_and_namespace_aliases_do_not_invent_missing_zeros() {
    let packet = format!(
        r#"<r:Description xmlns:r="{RDF}" xmlns="{CRS}"><ProcessVersion>15.4</ProcessVersion><Contrast2012>11</Contrast2012><Highlights2012>-22</Highlights2012><Shadows2012>33</Shadows2012><Blacks2012>-44</Blacks2012><Vibrance>55</Vibrance><Saturation>-66</Saturation></r:Description>"#
    );
    let values = xmp::parse_values(&packet).unwrap();
    assert_eq!(
        values.basic,
        BasicAdjustments {
            contrast: Some(11),
            highlights: Some(-22),
            shadows: Some(33),
            whites: None,
            blacks: Some(-44),
            vibrance: Some(55),
            saturation: Some(-66)
        }
    );
    let edited = xmp::patch(
        &packet,
        &Updates {
            basic: BasicAdjustments {
                saturation: Some(0),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    assert!(edited.contains("<Saturation>0</Saturation>"));
    assert!(edited.contains("<Vibrance>55</Vibrance>"));
    assert_eq!(xmp::parse_values(&edited).unwrap().basic.whites, None);
    let wrong_namespace =
        format!(r#"<r:Description xmlns:r="{RDF}" xmlns:c="urn:other" c:Contrast2012="99"/>"#);
    assert!(xmp::parse_values(&wrong_namespace).unwrap().basic.empty());
}

#[test]
fn legacy_basic_edits_use_old_contrast_and_do_not_alias_new_tonal_controls() {
    for version in ["5.0", "5.7"] {
        let packet = attributes(&format!(
            r#"crs:ProcessVersion="{version}" crs:Contrast="25" crs:AutoContrast="True" crs:Brightness="50" crs:Shadows="7" crs:Contrast2012="-10" crs:Highlights2012="-20" crs:Vibrance="4" crs:Saturation="-2""#
        ));
        let values = xmp::parse_values(&packet).unwrap();
        assert_eq!(
            values.basic,
            BasicAdjustments {
                contrast: Some(25),
                vibrance: Some(4),
                saturation: Some(-2),
                ..Default::default()
            }
        );
        assert_eq!(
            xmp::basic_range(&packet, BasicControl::Contrast).unwrap(),
            -50..=100
        );
        let edited = xmp::patch(
            &packet,
            &Updates {
                basic: BasicAdjustments {
                    contrast: Some(-25),
                    vibrance: Some(10),
                    saturation: Some(0),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        assert!(edited.contains(r#"crs:Contrast="-25""#));
        assert!(edited.contains(r#"crs:AutoContrast="False""#));
        assert!(edited.contains(
            r#"crs:Brightness="50" crs:Shadows="7" crs:Contrast2012="-10" crs:Highlights2012="-20""#
        ));
        assert_eq!(
            xmp::parse_values(&edited)
                .unwrap()
                .process_version
                .as_deref(),
            Some(version)
        );
        for control in [
            BasicControl::Highlights,
            BasicControl::Shadows,
            BasicControl::Whites,
            BasicControl::Blacks,
        ] {
            assert!(xmp::basic_range(&packet, control).is_err());
            let mut updates = Updates::default();
            updates.basic.set(control, Some(0));
            assert!(
                xmp::patch(&packet, &updates).is_err(),
                "{control:?} must not change the process"
            );
        }
        assert!(
            xmp::patch(
                &packet,
                &Updates {
                    basic: BasicAdjustments {
                        contrast: Some(-51),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
}

#[test]
fn unversioned_basic_inference_and_unknown_process_color_controls_are_safe() {
    let old = attributes(r#"crs:Contrast="12""#);
    assert_eq!(
        xmp::basic_range(&old, BasicControl::Contrast).unwrap(),
        -50..=100
    );
    assert_eq!(xmp::exposure_range(&old).unwrap(), -4_000_000..=4_000_000);
    let modern = attributes(r#"crs:Contrast2012="12""#);
    assert_eq!(
        xmp::basic_range(&modern, BasicControl::Contrast).unwrap(),
        -100..=100
    );
    for packet in [
        attributes(r#"crs:Contrast="12" crs:Contrast2012="24" crs:Vibrance="3""#),
        attributes(r#"crs:ProcessVersion="unknown" crs:Contrast2012="24" crs:Vibrance="3""#),
    ] {
        assert_eq!(xmp::parse_values(&packet).unwrap().basic.contrast, None);
        assert_eq!(xmp::parse_values(&packet).unwrap().basic.vibrance, Some(3));
        assert!(xmp::basic_range(&packet, BasicControl::Contrast).is_err());
        assert_eq!(
            xmp::basic_range(&packet, BasicControl::Vibrance).unwrap(),
            -100..=100
        );
        assert!(
            xmp::patch(
                &packet,
                &Updates {
                    basic: BasicAdjustments {
                        contrast: Some(0),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            )
            .is_err()
        );
        let edited = xmp::patch(
            &packet,
            &Updates {
                basic: BasicAdjustments {
                    vibrance: Some(0),
                    saturation: Some(8),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        assert!(edited.contains(r#"crs:Vibrance="0""#));
        assert_eq!(
            xmp::parse_values(&edited).unwrap().basic.saturation,
            Some(8)
        );
    }
}

#[test]
fn invalid_basic_edits_or_malformed_values_do_not_write_a_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("basics.ARW");
    let source =
        attributes(r#"crs:ProcessVersion="15.4" crs:Contrast2012="25" crs:UnknownFuture="keep""#);
    fs::write(raw.with_extension("xmp"), &source).unwrap();
    for control in BasicControl::ALL {
        for value in [-101, 101] {
            let mut update = Updates::default();
            update.basic.set(control, Some(value));
            assert!(!update.basic.valid());
            assert!(xmp::write(&raw, &update).is_err());
            assert_eq!(
                fs::read_to_string(raw.with_extension("xmp")).unwrap(),
                source
            );
        }
    }
    for value in ["1.5", "NaN", "--1", "2147483648", "101"] {
        assert!(
            xmp::parse_values(&attributes(&format!(
                r#"crs:ProcessVersion="15.4" crs:Contrast2012="{value}""#
            )))
            .is_err()
        );
    }
    let legacy = attributes(r#"crs:ProcessVersion="5.7" crs:Contrast="25""#);
    fs::write(raw.with_extension("xmp"), &legacy).unwrap();
    assert!(
        xmp::write(
            &raw,
            &Updates {
                basic: BasicAdjustments {
                    whites: Some(0),
                    ..Default::default()
                },
                ..Default::default()
            }
        )
        .is_err()
    );
    assert_eq!(
        fs::read_to_string(raw.with_extension("xmp")).unwrap(),
        legacy
    );
}

#[test]
fn basic_receipts_and_complete_develop_merge_keep_explicit_changes_and_new_source_decisions() {
    let basic = BasicAdjustments {
        contrast: Some(0),
        highlights: Some(-10),
        shadows: Some(20),
        whites: Some(30),
        blacks: Some(-40),
        vibrance: Some(50),
        saturation: Some(-60),
    };
    assert!(basic.valid() && !basic.empty());
    let updates = Updates {
        basic,
        exposure: Some(250_000),
        white_balance: Some(WhiteBalance::AsShot),
        ..Default::default()
    };
    let receipt = xmp::patch(&xmp::template(), &updates).unwrap();
    let values = xmp::parse_values(&receipt).unwrap();
    assert_eq!(values.basic, basic);
    assert_eq!(values.exposure, Some(250_000));
    assert_eq!(
        values.white_balance.unwrap().edit(),
        Some(WhiteBalance::AsShot)
    );
    assert_eq!(receipt.matches("crs:HasSettings=").count(), 1);
    let source = format!(
        r#"<r:Description xmlns:r="{RDF}" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:sr="urn:zrtc:syncratings:1/" xmp:Rating="5" xmp:Label="Red" sr:Flag="pick"/>"#
    );
    let baseline = format!(
        r#"<r:Description xmlns:r="{RDF}" xmlns:c="{CRS}" xmlns:xmp="http://ns.adobe.com/xap/1.0/" c:ProcessVersion="15.4" c:Contrast2012="75" xmp:Rating="1"><c:FutureMask><r:Seq><r:li c:Resource="asset.acr"/></r:Seq></c:FutureMask></r:Description>"#
    );
    let adobe = xmp::patch(&baseline, &updates).unwrap();
    let merged = xmp::merge_develop(&source, &adobe).unwrap();
    let values = xmp::parse_values(&merged).unwrap();
    assert_eq!(values.basic, basic);
    assert_eq!(values.rating, Some(5));
    assert_eq!(values.label.as_deref(), Some("Red"));
    assert_eq!(values.flag.as_deref(), Some("pick"));
    assert!(
        merged.contains(
            r#"<c:FutureMask><r:Seq><r:li c:Resource="asset.acr"/></r:Seq></c:FutureMask>"#
        )
    );
}

#[test]
fn basic_zero_is_a_real_saved_edit_while_absent_controls_stay_missing() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("new-basic.NEF");
    let updates = Updates {
        basic: BasicAdjustments {
            contrast: Some(0),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(!updates.zero());
    assert!(
        fastcull::state::apply_one(&fastcull::state::Item {
            path: raw.clone(),
            updates
        })
        .unwrap()
    );
    let values = xmp::read(&raw).unwrap();
    assert_eq!(values.basic.contrast, Some(0));
    assert_eq!(values.basic.highlights, None);
    assert_eq!(values.basic.saturation, None);
    assert!(
        xmp::packet(&raw)
            .unwrap()
            .unwrap()
            .contains(r#"crs:HasSettings="True""#)
    );
}
