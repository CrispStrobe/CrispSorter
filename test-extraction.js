import fs from 'fs';
import * as pdfjsLib from 'pdfjs-dist/legacy/build/pdf.mjs';
import mammoth from 'mammoth';

async function testPdf(filePath) {
    console.log(`\n--- Testing PDF: ${filePath} ---`);
    const data = new Uint8Array(fs.readFileSync(filePath));
    
    // For Node.js with legacy build, we still need to handle worker
    // But sometimes it works directly if disableWorker is true or in legacy
    const loadingTask = pdfjsLib.getDocument({ 
        data: data,
        disableFontFace: true, 
        verbosity: 0,
    });
    
    try {
        const pdfDocument = await loadingTask.promise;
        console.log(`Pages: ${pdfDocument.numPages}`);
        
        const page = await pdfDocument.getPage(1);
        const textContent = await page.getTextContent();
        
        const pageText = textContent.items
            .map((item) => {
                if ('str' in item) {
                    return item.str;
                }
                return '';
            })
            .join(' ');
        
        console.log('Extraction Sample (Page 1):');
        console.log(pageText.substring(0, 500).replace(/\n/g, ' ') + '...');
    } catch (e) {
        console.error(`PDF test error for ${filePath}: ${e.message}`);
    }
}

async function testDocx(filePath) {
    console.log(`\n--- Testing DOCX: ${filePath} ---`);
    const buffer = fs.readFileSync(filePath);
    try {
        const result = await mammoth.extractRawText({ buffer: buffer });
        console.log('Extraction Sample:');
        console.log(result.value.substring(0, 500).replace(/\n/g, ' ') + '...');
        if (result.messages.length > 0) {
            console.log('Messages:', result.messages);
        }
    } catch (e) {
        console.error(`DOCX test error for ${filePath}: ${e.message}`);
    }
}

async function run() {
    try {
        if (fs.existsSync('./samples/test.pdf')) {
            await testPdf('./samples/test.pdf');
        } else {
            console.log('PDF sample not found.');
        }
        if (fs.existsSync('./samples/test.docx')) {
            await testDocx('./samples/test.docx');
        } else {
            console.log('DOCX sample not found.');
        }
    } catch (error) {
        console.error('Test runner failed:', error);
    }
}

run();
